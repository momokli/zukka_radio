//! Integration tests for the playback API (Issue #13).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets. The token env var is
//! set exactly once via `std::sync::Once` (edition 2024: `set_var` is unsafe)
//! with a fixed value, so parallel tests cannot race on it.

use std::sync::Once;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const TEST_TOKEN: &str = "test-liquidsoap-token";

static INIT: Once = Once::new();

/// Sets the token env exactly once.
fn init_token() {
    INIT.call_once(|| {
        // SAFETY: the value is a fixed constant and every test funnels through
        // this `Once`, so no other thread observes a torn environment here.
        unsafe { std::env::set_var("LIQUIDSOAP_TOKEN", TEST_TOKEN) };
    });
}

async fn setup() -> (TempDir, SqlitePool, Router) {
    init_token();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    let app = zukka_scheduler::api::router(pool.clone());
    (dir, pool, app)
}

/// Sends one request, optionally with the token header, decoding the JSON body.
async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("x-liquidsoap-token", token);
    }
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", mime::APPLICATION_JSON.as_ref());
            builder
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Authenticated request with the fixed test token.
async fn send_auth(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send(app, method, uri, body, Some(TEST_TOKEN)).await
}

// ---------------------------------------------------------------------------
// Seed helpers (direct SQL — no API dependency)
// ---------------------------------------------------------------------------

async fn seed_caster(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("user_{}", id.replace('-', "")))
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
}

async fn seed_track(
    pool: &SqlitePool,
    id: &str,
    music_api_id: &str,
    title: &str,
    artist: &str,
    duration_ms: i64,
) {
    sqlx::query(
        "INSERT INTO tracks (id, music_api_id, title, artist, duration_ms, storage_key, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 's3/key', ?, ?)",
    )
    .bind(id)
    .bind(music_api_id)
    .bind(title)
    .bind(artist)
    .bind(duration_ms)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert track");
}

async fn seed_tape(
    pool: &SqlitePool,
    id: &str,
    slug: &str,
    caster_id: &str,
    title: &str,
    music_api_id: &str,
    duration_ms: i64,
) {
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, title, started_at, ended_at, duration_ms, source, music_api_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'upload', ?, ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(caster_id)
    .bind(title)
    .bind(TS)
    .bind(TS)
    .bind(duration_ms)
    .bind(music_api_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");
}

async fn seed_queue(pool: &SqlitePool, id: &str, caster_id: &str, name: &str, kind: &str) {
    sqlx::query(
        "INSERT INTO queues (id, caster_id, name, description, kind, version, created_at, updated_at) \
         VALUES (?, ?, ?, '', ?, 1, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(name)
    .bind(kind)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue");
}

#[allow(clippy::too_many_arguments)]
async fn seed_item(
    pool: &SqlitePool,
    id: &str,
    queue_id: &str,
    position: i64,
    kind: &str,
    track_id: Option<&str>,
    tape_id: Option<&str>,
    link_url: Option<&str>,
    title_override: Option<&str>,
    state: &str,
    played_at: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO queue_items \
         (id, queue_id, position, kind, track_id, tape_id, link_url, link_state, title_override, state, played_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(queue_id)
    .bind(position)
    .bind(kind)
    .bind(track_id)
    .bind(tape_id)
    .bind(link_url)
    .bind((kind == "link").then_some("pending"))
    .bind(title_override)
    .bind(state)
    .bind(played_at)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue item");
}

#[allow(clippy::too_many_arguments)]
async fn seed_broadcast(
    pool: &SqlitePool,
    id: &str,
    caster_id: &str,
    kind: &str,
    status: &str,
    tape_id: Option<&str>,
    queue_id: Option<&str>,
    starts_at: &str,
) {
    sqlx::query(
        "INSERT INTO broadcasts \
         (id, caster_id, kind, title, description, starts_at, status, tape_id, queue_id, version, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, 'B', '', ?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(kind)
    .bind(starts_at)
    .bind(status)
    .bind(tape_id)
    .bind(queue_id)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert broadcast");
}

async fn seed_setting(pool: &SqlitePool, key: &str, value: &str) {
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
        .bind(key)
        .bind(value)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert setting");
}

async fn item_state(pool: &SqlitePool, id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, played_at FROM queue_items WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("fetch item state")
}

// ---------------------------------------------------------------------------
// 2. Auth pure-unit matrix
// ---------------------------------------------------------------------------

#[test]
fn check_token_pure_matrix() {
    use zukka_scheduler::api::auth::check_token;
    // unset env → always false
    assert!(!check_token(None, None));
    assert!(!check_token(None, Some("x")));
    // empty env → always false
    assert!(!check_token(Some(""), Some("")));
    assert!(!check_token(Some(""), Some("token")));
    // configured, header missing / mismatch → false
    assert!(!check_token(Some("secret"), None));
    assert!(!check_token(Some("secret"), Some("other")));
    // configured, exact match → true
    assert!(check_token(Some("secret"), Some("secret")));
}

// ---------------------------------------------------------------------------
// 1. Auth fail-closed (integration)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_fail_closed() {
    let (_dir, _pool, app) = setup().await;

    // missing header → 401 unauthorized
    let (status, body) = send(&app, Method::GET, "/api/v1/playback/current", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    // wrong token → 401
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/v1/playback/current",
        None,
        Some("wrong"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    // correct token → 204 (no items yet)
    let (status, _) = send(
        &app,
        Method::GET,
        "/api/v1/playback/current",
        None,
        Some(TEST_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // POST is guarded too
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": "x" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// 3. Only `/playback/*` is protected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_playback_is_protected() {
    let (_dir, _pool, app) = setup().await;

    let (status, _) = send(&app, Method::GET, "/api/v1/queues", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(&app, Method::GET, "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 4. layer=queue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn next_layer_queue_via_live_playlist_broadcast() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000001",
        "m1",
        "Song A",
        "Artist A",
        215000,
    )
    .await;
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000001",
        caster,
        "slot1",
        "slot",
    )
    .await;
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000001",
        "a0000000-0000-7000-8000-000000000001",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000001"),
        None,
        None,
        None,
        "pending",
        None,
    )
    .await;
    seed_broadcast(
        &pool,
        "e0000000-0000-7000-8000-000000000001",
        caster,
        "playlist",
        "live",
        None,
        Some("a0000000-0000-7000-8000-000000000001"),
        TS,
    )
    .await;

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "queue next: {body}");
    assert_eq!(body["kind"], "track");
    assert_eq!(
        body["queue_item_id"],
        "b0000000-0000-7000-8000-000000000001"
    );
    assert_eq!(body["track_id"], "d0000000-0000-7000-8000-000000000001");
    assert_eq!(body["title"], "Song A");
    assert_eq!(body["artist"], "Artist A");
    assert_eq!(body["duration_ms"], 215000);
    assert!(
        body["uri"].as_str().unwrap().ends_with("/stream/tracks/m1"),
        "uri: {body}"
    );
}

#[tokio::test]
async fn next_layer_queue_settings_fallback_and_empty() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000002",
        "m2",
        "Song B",
        "Artist B",
        1000,
    )
    .await;
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000003",
        caster,
        "cbg",
        "background",
    )
    .await;
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000002",
        "a0000000-0000-7000-8000-000000000003",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000002"),
        None,
        None,
        None,
        "pending",
        None,
    )
    .await;
    seed_setting(
        &pool,
        "queue.active_id",
        "a0000000-0000-7000-8000-000000000003",
    )
    .await;

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["queue_item_id"],
        "b0000000-0000-7000-8000-000000000002"
    );

    // nothing pending → 204
    sqlx::query("UPDATE queue_items SET state = 'played' WHERE id = ?")
        .bind("b0000000-0000-7000-8000-000000000002")
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // settings points at a nonexistent queue → 204
    sqlx::query("UPDATE settings SET value = 'does-not-exist' WHERE key = 'queue.active_id'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// 5. layer=show
// ---------------------------------------------------------------------------

#[tokio::test]
async fn next_layer_show() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;

    // no broadcast → 204
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=show", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // a live `live`-kind broadcast → 204 (show has nothing)
    seed_broadcast(
        &pool,
        "e0000000-0000-7000-8000-000000000010",
        caster,
        "live",
        "live",
        None,
        None,
        TS,
    )
    .await;
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=show", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = ?")
        .bind("e0000000-0000-7000-8000-000000000010")
        .execute(&pool)
        .await
        .unwrap();

    // a live `recording` broadcast → its tape
    seed_tape(
        &pool,
        "70000000-0000-7000-8000-000000000001",
        "TAPE0001",
        caster,
        "Show Tape",
        "t-m1",
        3_600_000,
    )
    .await;
    seed_broadcast(
        &pool,
        "e0000000-0000-7000-8000-000000000011",
        caster,
        "recording",
        "live",
        Some("70000000-0000-7000-8000-000000000001"),
        None,
        TS,
    )
    .await;
    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=show", None).await;
    assert_eq!(status, StatusCode::OK, "show recording: {body}");
    assert_eq!(body["kind"], "tape");
    assert_eq!(body["queue_item_id"], Value::Null);
    assert_eq!(body["tape_id"], "70000000-0000-7000-8000-000000000001");
    assert_eq!(body["artist"], Value::Null);
    assert!(
        body["uri"]
            .as_str()
            .unwrap()
            .ends_with("/stream/tapes/t-m1"),
        "uri: {body}"
    );
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = ?")
        .bind("e0000000-0000-7000-8000-000000000011")
        .execute(&pool)
        .await
        .unwrap();

    // a live `playlist` broadcast → next queue item
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000010",
        caster,
        "showq",
        "slot",
    )
    .await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000010",
        "m10",
        "Show Song",
        "Artist 10",
        5000,
    )
    .await;
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000010",
        "a0000000-0000-7000-8000-000000000010",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000010"),
        None,
        None,
        None,
        "pending",
        None,
    )
    .await;
    seed_broadcast(
        &pool,
        "e0000000-0000-7000-8000-000000000012",
        caster,
        "playlist",
        "live",
        None,
        Some("a0000000-0000-7000-8000-000000000010"),
        TS,
    )
    .await;
    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=show", None).await;
    assert_eq!(status, StatusCode::OK, "show playlist: {body}");
    assert_eq!(
        body["queue_item_id"],
        "b0000000-0000-7000-8000-000000000010"
    );
}

// ---------------------------------------------------------------------------
// 6. layer validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn layer_validation() {
    let (_dir, _pool, app) = setup().await;
    for layer in ["bogus", "rotation", "live"] {
        let (status, body) = send_auth(
            &app,
            Method::GET,
            &format!("/api/v1/playback/next?layer={layer}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "layer={layer}");
        assert_eq!(body["error"]["code"], "validation_error");
        assert_eq!(body["error"]["details"]["field"], "layer");
    }
}

// ---------------------------------------------------------------------------
// 7. Projection: track, tape, title_override
// ---------------------------------------------------------------------------

#[tokio::test]
async fn projection_track_tape_and_title_override() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000020",
        "m20",
        "Orig Title",
        "Orig Artist",
        1234,
    )
    .await;
    seed_tape(
        &pool,
        "70000000-0000-7000-8000-000000000020",
        "TAPE0020",
        caster,
        "Tape Title",
        "t-m20",
        2222,
    )
    .await;
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000020",
        caster,
        "projq",
        "background",
    )
    .await;
    seed_setting(
        &pool,
        "queue.active_id",
        "a0000000-0000-7000-8000-000000000020",
    )
    .await;
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000020",
        "a0000000-0000-7000-8000-000000000020",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000020"),
        None,
        None,
        Some("Override"),
        "pending",
        None,
    )
    .await;

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Override");
    assert_eq!(body["artist"], "Orig Artist");
    assert_eq!(body["duration_ms"], 1234);
    assert!(
        body["uri"]
            .as_str()
            .unwrap()
            .ends_with("/stream/tracks/m20"),
        "uri: {body}"
    );

    // tape item at position 1; no override → tape title, artist null
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000021",
        "a0000000-0000-7000-8000-000000000020",
        1,
        "tape",
        None,
        Some("70000000-0000-7000-8000-000000000020"),
        None,
        None,
        "pending",
        None,
    )
    .await;
    sqlx::query("UPDATE queue_items SET state = 'played' WHERE id = ?")
        .bind("b0000000-0000-7000-8000-000000000020")
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["kind"], "tape");
    assert_eq!(body["title"], "Tape Title");
    assert_eq!(body["artist"], Value::Null);
    assert_eq!(body["tape_id"], "70000000-0000-7000-8000-000000000020");
    assert_eq!(body["duration_ms"], 2222);
    assert!(
        body["uri"]
            .as_str()
            .unwrap()
            .ends_with("/stream/tapes/t-m20"),
        "uri: {body}"
    );
}

// ---------------------------------------------------------------------------
// 8. link items are skipped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn link_item_is_skipped() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000030",
        "m30",
        "After Link",
        "A",
        111,
    )
    .await;
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000030",
        caster,
        "linkq",
        "background",
    )
    .await;
    seed_setting(
        &pool,
        "queue.active_id",
        "a0000000-0000-7000-8000-000000000030",
    )
    .await;
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000030",
        "a0000000-0000-7000-8000-000000000030",
        0,
        "link",
        None,
        None,
        Some("https://example.com/x"),
        None,
        "pending",
        None,
    )
    .await;
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000031",
        "a0000000-0000-7000-8000-000000000030",
        1,
        "track",
        Some("d0000000-0000-7000-8000-000000000030"),
        None,
        None,
        None,
        "pending",
        None,
    )
    .await;

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["queue_item_id"],
        "b0000000-0000-7000-8000-000000000031"
    );
    assert_eq!(body["kind"], "track");
}

// ---------------------------------------------------------------------------
// 9. Idempotent transitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transitions_idempotent() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000040",
        caster,
        "tq",
        "background",
    )
    .await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000040",
        "m40",
        "T",
        "A",
        1,
    )
    .await;
    let item = "b0000000-0000-7000-8000-000000000040";
    seed_item(
        &pool,
        item,
        "a0000000-0000-7000-8000-000000000040",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000040"),
        None,
        None,
        None,
        "pending",
        None,
    )
    .await;

    // started → playing
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": item, "kind": "track" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(item_state(&pool, item).await.0, "playing");

    // repeat started → 204, unchanged
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": item })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(item_state(&pool, item).await.0, "playing");

    // ended → played + played_at
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": item })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (state, played_at) = item_state(&pool, item).await;
    assert_eq!(state, "played");
    let first_played = played_at.expect("played_at is set");

    // repeat ended → 204, played_at unchanged
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": item })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (state, played_at2) = item_state(&pool, item).await;
    assert_eq!(state, "played");
    assert_eq!(played_at2, Some(first_played));

    // error on a fresh item → skipped, repeat → no-op
    let item2 = "b0000000-0000-7000-8000-000000000041";
    seed_item(
        &pool,
        item2,
        "a0000000-0000-7000-8000-000000000040",
        1,
        "track",
        Some("d0000000-0000-7000-8000-000000000040"),
        None,
        None,
        None,
        "pending",
        None,
    )
    .await;
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/error",
        Some(json!({ "queue_item_id": item2, "error": "boom" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(item_state(&pool, item2).await.0, "skipped");
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/error",
        Some(json!({ "queue_item_id": item2 })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(item_state(&pool, item2).await.0, "skipped");
}

// ---------------------------------------------------------------------------
// 10. Unknown item / missing field
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_item_and_missing_field() {
    let (_dir, _pool, app) = setup().await;
    let ghost = "ffffffff-ffff-7fff-8fff-ffffffffffff";

    // unknown queue_item_id → 404 not_found
    let (status, body) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": ghost })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    // missing queue_item_id → 422 validation_error
    let (status, body) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "queue_item_id");

    // invalid kind → 422 (parsed before the existence check)
    let (status, body) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": ghost, "kind": "link" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "kind");
}

// ---------------------------------------------------------------------------
// 11. /playback/current
// ---------------------------------------------------------------------------

#[tokio::test]
async fn current_resolution() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    seed_caster(&pool, caster).await;
    seed_queue(
        &pool,
        "a0000000-0000-7000-8000-000000000050",
        caster,
        "cq",
        "background",
    )
    .await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000050",
        "m50",
        "Cur",
        "A",
        9,
    )
    .await;

    // empty → 204
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // no playing, but a played_at entry → last reported
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000050",
        "a0000000-0000-7000-8000-000000000050",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000050"),
        None,
        None,
        None,
        "played",
        Some("2026-11-18T12:00:00.000Z"),
    )
    .await;
    let (status, body) = send_auth(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["queue_item_id"],
        "b0000000-0000-7000-8000-000000000050"
    );

    // a playing item wins over the last reported one
    seed_item(
        &pool,
        "b0000000-0000-7000-8000-000000000051",
        "a0000000-0000-7000-8000-000000000050",
        1,
        "track",
        Some("d0000000-0000-7000-8000-000000000050"),
        None,
        None,
        None,
        "playing",
        None,
    )
    .await;
    let (status, body) = send_auth(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["queue_item_id"],
        "b0000000-0000-7000-8000-000000000051"
    );
}
