//! Store + HTTP tests for waveforms (Issue #18: D8/C05).
//!
//! Store tests run `zukka_scheduler::store::waveforms` directly against a
//! migrated temporary SQLite database. API tests go through
//! `zukka_scheduler::api::router` with `tower::ServiceExt::oneshot`; the
//! `/internal/*` routes require the service token (§4.1).

use std::sync::OnceLock;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::api::auth as api_auth;
use zukka_scheduler::dto::UpsertWaveformRequest;
use zukka_scheduler::store::waveforms;
use zukka_scheduler::uuid::new_id;

fn service_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "waveform-test-token".to_string();
        // SAFETY: set once for the whole test binary, before any request.
        unsafe { std::env::set_var(api_auth::SERVICE_TOKEN_ENV, &token) };
        token
    })
}

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    (dir, pool)
}

fn waveform(subject_id: &str) -> UpsertWaveformRequest {
    UpsertWaveformRequest {
        subject_kind: "track".into(),
        subject_id: subject_id.into(),
        scheme: 1,
        algorithm: "ffmpeg-minmax-8k/1".into(),
        ms_per_bar: 500,
        bars: 2,
        // 2 bars → 4 bytes: (min,max) pairs.
        data: vec![0x80, 0x90, 0x40, 0xB0],
    }
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    with_token: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if with_token {
        builder = builder.header("authorization", format!("Bearer {}", service_token()));
    }
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upsert_then_get_roundtrip() {
    let (_dir, pool) = setup().await;
    let id = new_id();
    let dto = waveforms::upsert(&pool, &waveform(&id))
        .await
        .expect("upsert");
    assert_eq!(dto.subject_kind, "track");
    assert_eq!(dto.subject_id, id);
    assert_eq!(dto.bars, 2);
    assert_eq!(dto.data, vec![0x80, 0x90, 0x40, 0xB0]);

    let fetched = waveforms::get(&pool, "track", &id).await.expect("get");
    assert_eq!(fetched.data, dto.data);
}

#[tokio::test]
async fn upsert_replaces_existing_row() {
    let (_dir, pool) = setup().await;
    let id = new_id();
    waveforms::upsert(&pool, &waveform(&id)).await.unwrap();

    let mut second = waveform(&id);
    second.data = vec![1, 2, 3, 4];
    second.algorithm = "ffmpeg-minmax-8k/2".into();
    let dto = waveforms::upsert(&pool, &second).await.unwrap();
    assert_eq!(dto.data, vec![1, 2, 3, 4]);
    assert_eq!(dto.algorithm, "ffmpeg-minmax-8k/2");

    // Still a single row (PK upsert).
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM waveforms")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn get_unknown_is_404() {
    let (_dir, pool) = setup().await;
    let err = waveforms::get(&pool, "track", "ghost").await.unwrap_err();
    assert!(matches!(
        err,
        zukka_scheduler::error::ApiError::NotFound { .. }
    ));
}

#[tokio::test]
async fn upsert_validates_fields() {
    let (_dir, pool) = setup().await;

    let mut bad_kind = waveform("t");
    bad_kind.subject_kind = "album".into();
    assert!(waveforms::upsert(&pool, &bad_kind).await.is_err());

    let mut bad_len = waveform("t");
    bad_len.data = vec![0, 1, 2]; // 3 != 2 * 2
    assert!(waveforms::upsert(&pool, &bad_len).await.is_err());

    let mut bad_scheme = waveform("t");
    bad_scheme.scheme = 0;
    assert!(waveforms::upsert(&pool, &bad_scheme).await.is_err());
}

#[tokio::test]
async fn tape_subject_kind_is_accepted() {
    let (_dir, pool) = setup().await;
    let id = new_id();
    let mut req = waveform(&id);
    req.subject_kind = "tape".into();
    let dto = waveforms::upsert(&pool, &req).await.expect("upsert tape");
    assert_eq!(dto.subject_kind, "tape");
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_requires_service_token() {
    let (_dir, pool) = setup().await;
    let id = new_id();
    let body = json!({
        "subject_kind": "track",
        "subject_id": id,
        "scheme": 1,
        "algorithm": "ffmpeg-minmax-8k/1",
        "ms_per_bar": 500,
        "bars": 2,
        "data": [128, 144, 64, 176],
    });
    let (status, _) = send(
        &app(pool),
        Method::POST,
        "/api/v1/internal/waveforms",
        Some(body),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_then_get_roundtrip() {
    let (_dir, pool) = setup().await;
    let id = new_id();
    let app = app(pool.clone());
    let body = json!({
        "subject_kind": "track",
        "subject_id": id,
        "scheme": 1,
        "algorithm": "ffmpeg-minmax-8k/1",
        "ms_per_bar": 500,
        "bars": 2,
        "data": [128, 144, 64, 176],
    });
    let (status, created) = send(
        &app,
        Method::POST,
        "/api/v1/internal/waveforms",
        Some(body),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["subject_id"], id);
    assert_eq!(created["data"], json!([128, 144, 64, 176]));

    let (status, fetched) = send(
        &app,
        Method::GET,
        &format!("/api/v1/internal/waveforms/track/{id}"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["data"], json!([128, 144, 64, 176]));
}

#[tokio::test]
async fn get_unknown_subject_is_404() {
    let (_dir, pool) = setup().await;
    let (status, _) = send(
        &app(pool),
        Method::GET,
        "/api/v1/internal/waveforms/track/0192aaaa-0000-7000-8000-000000000000",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn post_invalid_kind_is_422() {
    let (_dir, pool) = setup().await;
    let body = json!({
        "subject_kind": "album",
        "subject_id": "x",
        "scheme": 1,
        "algorithm": "a",
        "ms_per_bar": 0,
        "bars": 0,
        "data": [],
    });
    let (status, body) = send(
        &app(pool),
        Method::POST,
        "/api/v1/internal/waveforms",
        Some(body),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "subject_kind");
}
