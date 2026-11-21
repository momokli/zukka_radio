//! Integration tests for `GET /playback/rotation.m3u` (Issue #14).
//!
//! Runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no sockets. All env vars are set exactly
//! once via `std::sync::Once` (edition 2024: `set_var` is unsafe) with fixed
//! values so parallel tests cannot race on them.

use std::path::PathBuf;
use std::sync::Once;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const TEST_TOKEN: &str = "test-liquidsoap-token";
const TS: &str = "2026-11-18T12:00:00.000Z";

static INIT: Once = Once::new();

/// Directory holding the rotation file for this test binary (per-process).
fn rotation_dir() -> PathBuf {
    std::env::temp_dir().join(format!("zukka-rot-test-{}", std::process::id()))
}

/// Sets the env exactly once (token, Music-API base, rotation path).
fn init_env() {
    INIT.call_once(|| {
        let dir = rotation_dir();
        std::fs::create_dir_all(&dir).expect("create rotation test dir");
        let path = dir.join("rotation.m3u");
        // SAFETY: fixed constants, funnelled through a single `Once`, so no
        // other thread observes a torn environment here.
        unsafe {
            std::env::set_var("LIQUIDSOAP_TOKEN", TEST_TOKEN);
            std::env::set_var("MUSIC_API_BASE_URL", "http://music.test");
            std::env::set_var("ROTATION_M3U_PATH", &path);
        }
    });
}

async fn setup() -> (TempDir, SqlitePool, Router) {
    init_env();
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(&dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    let app = zukka_scheduler::api::router(pool.clone());
    (dir, pool, app)
}

/// Sends one raw request, returning `(status, content_type, body_string)`.
async fn get_raw(
    app: &Router,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Option<String>, String) {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-liquidsoap-token", token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("router response");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|value| value.to_str().unwrap().to_string());
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

async fn seed_track(
    pool: &SqlitePool,
    id: &str,
    music_api_id: &str,
    title: &str,
    artist: &str,
    duration_ms: i64,
    visibility: &str,
) {
    sqlx::query(
        "INSERT INTO tracks \
         (id, music_api_id, title, artist, duration_ms, storage_key, visibility, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 's3/key', ?, ?, ?)",
    )
    .bind(id)
    .bind(music_api_id)
    .bind(title)
    .bind(artist)
    .bind(duration_ms)
    .bind(visibility)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert track");
}

/// 1. shared-only projection with absolute URLs + EXTINF line.
#[tokio::test]
async fn valid_m3u_projection() {
    let (_dir, pool, app) = setup().await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000001",
        "m1",
        "Song A",
        "Artist A",
        215_000,
        "shared",
    )
    .await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000002",
        "m2",
        "Private",
        "Nobody",
        30_000,
        "private",
    )
    .await;

    let (status, ctype, body) =
        get_raw(&app, "/api/v1/playback/rotation.m3u", Some(TEST_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ctype.as_deref(), Some("audio/x-mpegurl"));
    assert!(body.starts_with("#EXTM3U\n"), "{body}");
    assert!(
        body.contains("http://music.test/objects/m1"),
        "absolute shared url missing: {body}"
    );
    assert!(body.contains("#EXTINF:215,Artist A - Song A"), "{body}");
    assert!(!body.contains("m2"), "private track leaked: {body}");
}

/// 2. empty rotation → exactly `#EXTM3U\n`.
#[tokio::test]
async fn empty_rotation() {
    let (_dir, _pool, app) = setup().await;
    let (status, ctype, body) =
        get_raw(&app, "/api/v1/playback/rotation.m3u", Some(TEST_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ctype.as_deref(), Some("audio/x-mpegurl"));
    assert_eq!(body, "#EXTM3U\n");
}

/// 3. token layer: missing / wrong token → 401 unauthorized.
#[tokio::test]
async fn token_missing_401() {
    let (_dir, _pool, app) = setup().await;
    for token in [None, Some("wrong")] {
        let (status, _ctype, body) = get_raw(&app, "/api/v1/playback/rotation.m3u", token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "token={token:?}");
        let value: Value = serde_json::from_str(&body).expect("error envelope");
        assert_eq!(value["error"]["code"], "unauthorized");
    }
}

/// 4. newlines in metadata are collapsed; no line break inside `#EXTINF`.
#[tokio::test]
async fn escaping_newlines() {
    let (_dir, pool, app) = setup().await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000010",
        "m10",
        "T\rC",
        "A\nB",
        5_000,
        "shared",
    )
    .await;

    let (status, _ctype, body) =
        get_raw(&app, "/api/v1/playback/rotation.m3u", Some(TEST_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("#EXTINF:5,A B - T C"), "{body}");
    assert!(!body.contains("A\nB"), "raw newline in artist: {body}");
    assert!(
        !body.contains("T\rC"),
        "raw carriage return in title: {body}"
    );
    // header + EXTINF + URL = 3 lines + trailing LF.
    assert_eq!(body.lines().count(), 3, "{body:?}");
}

/// 5. the file is written (content prefix check, race-safe).
#[tokio::test]
async fn writes_file_atomically() {
    let (_dir, pool, app) = setup().await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000020",
        "m20",
        "Atomic",
        "Artist",
        1_000,
        "shared",
    )
    .await;

    let (status, _ctype, _body) =
        get_raw(&app, "/api/v1/playback/rotation.m3u", Some(TEST_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);

    let contents = std::fs::read_to_string(rotation_dir().join("rotation.m3u"))
        .expect("rotation file exists after GET");
    assert!(contents.starts_with("#EXTM3U"), "{contents}");

    let leftovers: Vec<_> = std::fs::read_dir(rotation_dir())
        .expect("read rotation dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "unexpected temp files: {leftovers:?}");
}

/// 6. deterministic `ORDER BY id`.
#[tokio::test]
async fn order_deterministic() {
    let (_dir, pool, app) = setup().await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000031",
        "m31",
        "Second",
        "B",
        2_000,
        "shared",
    )
    .await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000030",
        "m30",
        "First",
        "A",
        1_000,
        "shared",
    )
    .await;

    let (status, _ctype, body) =
        get_raw(&app, "/api/v1/playback/rotation.m3u", Some(TEST_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let first = body.find("/objects/m30").expect("m30 present");
    let second = body.find("/objects/m31").expect("m31 present");
    assert!(first < second, "id order violated: {body}");
}
