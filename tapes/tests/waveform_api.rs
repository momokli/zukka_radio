//! HTTP tests for `GET|HEAD /api/v1/tapes/{slug}/waveform` (Issue #25, M3).
//!
//! The database is a temporary SQLite file seeded from the scheduler schema;
//! the application pool is read-only. Responses are checked byte-for-byte
//! against the ZKWV layout (`docs/tapes.md` §3.6).

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const BASE_URL: &str = "https://tapes.test";
const TS: &str = "2026-11-01T00:00:00.000Z";
const BARS: i64 = 3;
const DATA: [u8; 3] = [0, 128, 255];

fn build(method: Method, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    send(app, build(Method::GET, uri, &[])).await
}

async fn setup() -> (TempDir, Router, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");

    let writer = zukka_tapes::fixtures::create_db(&path)
        .await
        .expect("create db");

    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES ('c1', 'caster', 'Caster', 'x', \
         '0192f0a1-0001-7000-8000-000000000003', 1, ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&writer)
    .await
    .expect("caster");

    for (id, slug, status, visible, with_waveform) in [
        ("t-ready", "ready1", "ready", 1, true),
        ("t-hidden", "hidden1", "ready", 0, true),
        ("t-pending", "pending1", "pending", 1, false),
        ("t-nopeaks", "nopeaks1", "ready", 1, false),
    ] {
        sqlx::query(
            "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, status, \
             visible, format, mime, created_at, updated_at) VALUES (?, ?, 'c1', \
             '2026-11-20T10:00:00.000Z', '2026-11-20T11:00:00.000Z', 'live', ?, ?, 'mp3', \
             'audio/mpeg', ?, ?)",
        )
        .bind(id)
        .bind(slug)
        .bind(status)
        .bind(visible)
        .bind(TS)
        .bind(TS)
        .execute(&writer)
        .await
        .expect("tape");

        if with_waveform {
            sqlx::query(
                "INSERT INTO waveforms (subject_kind, subject_id, scheme, algorithm, ms_per_bar, \
                 bars, data, created_at) VALUES ('tape', ?, 1, 'ffmpeg-rms-500ms/1', 500, ?, ?, ?)",
            )
            .bind(id)
            .bind(BARS)
            .bind(DATA.to_vec())
            .bind(TS)
            .execute(&writer)
            .await
            .expect("waveform");
        }
    }

    writer.close().await;
    let pool = zukka_tapes::db::connect_read_only(&path)
        .await
        .expect("read-only pool");
    let app = zukka_tapes::api::router(pool.clone(), BASE_URL.to_string());
    (dir, app, pool)
}

#[tokio::test]
async fn ready_tape_returns_zkwv_with_headers() {
    let (_dir, app, _pool) = setup().await;
    let (status, headers, body) = get(&app, "/api/v1/tapes/ready1/waveform").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap(),
        "application/octet-stream"
    );
    assert_eq!(headers.get("content-length").unwrap(), "15");
    assert_eq!(
        headers.get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(headers.get("etag").unwrap(), "\"w1-ready1-3\"");
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");

    assert_eq!(body.len(), 12 + BARS as usize);
    assert_eq!(&body[0..4], b"ZKWV");
    assert_eq!(body[4], 1);
    assert_eq!(body[5], 0);
    assert_eq!(&body[6..8], &[0, 0]);
    assert_eq!(&body[8..12], &(BARS as u32).to_le_bytes());
    assert_eq!(&body[12..], &DATA);
}

#[tokio::test]
async fn hidden_pending_unknown_and_missing_peaks_share_one_404() {
    let (_dir, app, _pool) = setup().await;
    let (_status, _headers, unknown) = get(&app, "/api/v1/tapes/does-not-exist/waveform").await;

    for slug in ["hidden1", "pending1", "nopeaks1"] {
        let (status, _headers, body) = get(&app, &format!("/api/v1/tapes/{slug}/waveform")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{slug}");
        assert_eq!(body, unknown, "{slug} must be byte-identical");
    }

    let value: Value = serde_json::from_slice(&unknown).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
    assert_eq!(value["error"]["message"], "tape not found");
}

#[tokio::test]
async fn matching_etag_returns_304_without_body() {
    let (_dir, app, _pool) = setup().await;
    let (_s, headers, _b) = get(&app, "/api/v1/tapes/ready1/waveform").await;
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();

    let (status, headers, body) = send(
        &app,
        build(
            Method::GET,
            "/api/v1/tapes/ready1/waveform",
            &[("if-none-match", &etag)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    assert_eq!(headers.get("etag").unwrap(), etag.as_str());
    assert_eq!(
        headers.get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
}

#[tokio::test]
async fn head_returns_headers_without_body() {
    let (_dir, app, _pool) = setup().await;
    let (status, headers, body) = send(
        &app,
        build(Method::HEAD, "/api/v1/tapes/ready1/waveform", &[]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(headers.get("content-length").unwrap(), "15");
    assert_eq!(headers.get("etag").unwrap(), "\"w1-ready1-3\"");
}

#[tokio::test]
async fn options_preflight_is_204_with_cors() {
    let (_dir, app, _pool) = setup().await;
    let (status, headers, body) = send(
        &app,
        build(Method::OPTIONS, "/api/v1/tapes/ready1/waveform", &[]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
    assert_eq!(
        headers.get("access-control-allow-methods").unwrap(),
        "GET, HEAD, OPTIONS"
    );
}

#[tokio::test]
async fn pool_remains_read_only() {
    let (_dir, _app, pool) = setup().await;
    let result = sqlx::query("UPDATE tapes SET title = 'x' WHERE id = 't-ready'")
        .execute(&pool)
        .await;
    assert!(result.is_err(), "waveform path must never write");
}
