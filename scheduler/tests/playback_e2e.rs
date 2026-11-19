//! End-to-end tests for the playback API (Issue #13).
//!
//! These drive the **whole flow** through `zukka_scheduler::api::router` via
//! `tower::ServiceExt::oneshot` (no sockets, no mocks): a live `playlist`
//! broadcast is created + started through the public `/broadcasts` API, and the
//! full `next → started → current → ended → next` lifecycle is walked over the
//! real `/playback/*` routes until the queue drains. Also covers idempotency,
//! fail-closed token auth on every route, and the negative paths.
//!
//! Prerequisites that have no creation API in the scheduler (casters, tracks,
//! tapes) are seeded with direct SQL — the same strategy as
//! `playback_api.rs` (plan §4). The token env var is set exactly once per test
//! binary via `std::sync::Once` (edition 2024: `set_var` is unsafe).

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

/// Sets the token env exactly once for this test binary.
fn init_token() {
    INIT.call_once(|| {
        // SAFETY: fixed constant, funneled through `Once`, so no other thread
        // observes a torn environment here.
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
// Seed helpers (direct SQL — prerequisites without a creation API)
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
) {
    sqlx::query(
        "INSERT INTO queue_items \
         (id, queue_id, position, kind, track_id, tape_id, link_url, link_state, title_override, state, played_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)",
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
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue item");
}

async fn item_state(pool: &SqlitePool, id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, played_at FROM queue_items WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("fetch item state")
}

/// Creates + starts a live `playlist` broadcast through the public API and
/// returns its id. Exercises `/broadcasts` + `/broadcasts/{id}/start` (which are
/// *not* token-guarded) so the playlist is genuinely "on air" for layer
/// resolution instead of being hand-inserted.
async fn create_live_playlist_broadcast(app: &Router, caster_id: &str, queue_id: &str) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/broadcasts",
        Some(json!({
            "caster_id": caster_id,
            "kind": "playlist",
            "title": "E2E Show",
            "starts_at": TS,
            "queue_id": queue_id,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create broadcast: {body}");
    let id = body["id"].as_str().expect("broadcast id").to_string();

    let (status, body) = send(
        app,
        Method::POST,
        &format!("/api/v1/broadcasts/{id}/start"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start broadcast: {body}");
    assert_eq!(body["status"], "live");
    id
}

// ---------------------------------------------------------------------------
// 1. Full lifecycle over the real API: next → started → current → ended
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_full_playback_lifecycle() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    let queue = "a0000000-0000-7000-8000-000000000001";
    let link = "b0000000-0000-7000-8000-000000000001";
    let track_item = "b0000000-0000-7000-8000-000000000002";
    let tape_item = "b0000000-0000-7000-8000-000000000003";

    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000001",
        "m1",
        "Intro Song",
        "Artist A",
        215000,
    )
    .await;
    seed_tape(
        &pool,
        "70000000-0000-7000-8000-000000000001",
        "TAPE0001",
        caster,
        "Recorded Set",
        "t-m1",
        3_600_000,
    )
    .await;
    seed_queue(&pool, queue, caster, "slot1", "slot").await;
    // link first (must be skipped), then a track, then a tape.
    seed_item(
        &pool,
        link,
        queue,
        0,
        "link",
        None,
        None,
        Some("https://example.com/x"),
        None,
        "pending",
    )
    .await;
    seed_item(
        &pool,
        track_item,
        queue,
        1,
        "track",
        Some("d0000000-0000-7000-8000-000000000001"),
        None,
        None,
        None,
        "pending",
    )
    .await;
    seed_item(
        &pool,
        tape_item,
        queue,
        2,
        "tape",
        None,
        Some("70000000-0000-7000-8000-000000000001"),
        None,
        None,
        "pending",
    )
    .await;

    create_live_playlist_broadcast(&app, caster, queue).await;

    // 1) next (queue) → first playable pending item; the link is skipped.
    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "next #1: {body}");
    assert_eq!(body["queue_item_id"], track_item);
    assert_eq!(body["kind"], "track");
    assert_eq!(body["title"], "Intro Song");

    // 2) started → 204; current reflects the same item.
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": track_item, "kind": "track", "layer": "queue" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send_auth(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(status, StatusCode::OK, "current: {body}");
    assert_eq!(body["queue_item_id"], track_item);

    // 3) ended → 204; next (queue) → the tape item.
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": track_item })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (state, played_at) = item_state(&pool, track_item).await;
    assert_eq!(state, "played");
    assert!(played_at.is_some(), "played_at set after ended");

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "next #2: {body}");
    assert_eq!(body["queue_item_id"], tape_item);
    assert_eq!(body["kind"], "tape");
    assert_eq!(body["artist"], Value::Null);

    // 4) drain the tape too → queue is empty → 204.
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/started",
        Some(json!({ "queue_item_id": tape_item, "kind": "tape" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send_auth(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(status, StatusCode::OK, "current tape: {body}");
    assert_eq!(body["queue_item_id"], tape_item);
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": tape_item })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The skipped link stayed pending (never advanced).
    assert_eq!(item_state(&pool, link).await.0, "pending");
}

// ---------------------------------------------------------------------------
// 1b. Same lifecycle, but with `settings['queue.active_id']` as the source of
//     the active queue (no live broadcast) — proves the fallback path end to
//     end through the API.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_lifecycle_via_settings_active_queue() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    let queue = "a0000000-0000-7000-8000-000000000010";
    let item = "b0000000-0000-7000-8000-000000000010";

    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000010",
        "m10",
        "Fallback Song",
        "A",
        1000,
    )
    .await;
    seed_queue(&pool, queue, caster, "cbg", "background").await;
    seed_item(
        &pool,
        item,
        queue,
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000010"),
        None,
        None,
        None,
        "pending",
    )
    .await;
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('queue.active_id', ?, ?)")
        .bind(queue)
        .bind(TS)
        .execute(&pool)
        .await
        .expect("insert setting");

    let (status, body) =
        send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queue_item_id"], item);

    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/error",
        Some(json!({ "queue_item_id": item, "error": "decode failed" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(item_state(&pool, item).await.0, "skipped");

    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// 2. Idempotency of started / ended / error (double-dispatch)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_transitions_idempotent_double_dispatch() {
    let (_dir, pool, app) = setup().await;
    let caster = "c0000000-0000-7000-8000-000000000001";
    let queue = "a0000000-0000-7000-8000-000000000020";
    let a = "b0000000-0000-7000-8000-000000000020";
    let b = "b0000000-0000-7000-8000-000000000021";

    seed_caster(&pool, caster).await;
    seed_track(
        &pool,
        "d0000000-0000-7000-8000-000000000020",
        "m20",
        "T",
        "A",
        1,
    )
    .await;
    seed_queue(&pool, queue, caster, "iq", "background").await;
    seed_item(
        &pool,
        a,
        queue,
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000020"),
        None,
        None,
        None,
        "pending",
    )
    .await;
    seed_item(
        &pool,
        b,
        queue,
        1,
        "track",
        Some("d0000000-0000-7000-8000-000000000020"),
        None,
        None,
        None,
        "pending",
    )
    .await;

    // started twice → both 204, state playing, no played_at.
    for _ in 0..2 {
        let (status, _) = send_auth(
            &app,
            Method::POST,
            "/api/v1/playback/started",
            Some(json!({ "queue_item_id": a })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (state, played_at) = item_state(&pool, a).await;
    assert_eq!(state, "playing");
    assert_eq!(played_at, None);

    // ended twice → both 204, played_at set once and not overwritten.
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": a })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (state, first) = item_state(&pool, a).await;
    assert_eq!(state, "played");
    let first = first.expect("played_at set");

    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": a, "ended_at": "2030-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (state, second) = item_state(&pool, a).await;
    assert_eq!(state, "played");
    assert_eq!(
        second.as_deref(),
        Some(first.as_str()),
        "played_at unchanged"
    );

    // error twice → both 204, skipped; a later ended is a no-op (stays skipped).
    for _ in 0..2 {
        let (status, _) = send_auth(
            &app,
            Method::POST,
            "/api/v1/playback/error",
            Some(json!({ "queue_item_id": b })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(item_state(&pool, b).await.0, "skipped");
    let (status, _) = send_auth(
        &app,
        Method::POST,
        "/api/v1/playback/ended",
        Some(json!({ "queue_item_id": b })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (state, played_at) = item_state(&pool, b).await;
    assert_eq!(state, "skipped");
    assert_eq!(played_at, None);
}

// ---------------------------------------------------------------------------
// 3. Auth: every /playback/* route is fail-closed; other routes stay open
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_auth_on_every_playback_route() {
    let (_dir, _pool, app) = setup().await;
    let ghost = "ffffffff-ffff-7fff-8fff-ffffffffffff";

    // (method, uri, optional body)
    let routes: Vec<(Method, &str, Option<Value>)> = vec![
        (Method::GET, "/api/v1/playback/next?layer=queue", None),
        (Method::GET, "/api/v1/playback/current", None),
        (
            Method::POST,
            "/api/v1/playback/started",
            Some(json!({ "queue_item_id": ghost })),
        ),
        (
            Method::POST,
            "/api/v1/playback/ended",
            Some(json!({ "queue_item_id": ghost })),
        ),
        (
            Method::POST,
            "/api/v1/playback/error",
            Some(json!({ "queue_item_id": ghost })),
        ),
    ];

    for (method, uri, body) in &routes {
        // missing token → 401
        let (status, resp) = send(&app, method.clone(), uri, body.clone(), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no token: {method} {uri}");
        assert_eq!(resp["error"]["code"], "unauthorized");

        // wrong token → 401
        let (status, resp) = send(
            &app,
            method.clone(),
            uri,
            body.clone(),
            Some("not-the-token"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "bad token: {method} {uri}"
        );
        assert_eq!(resp["error"]["code"], "unauthorized");
    }

    // with the correct token the guard passes: GETs are 204 (empty), POSTs hit
    // the handler and report 404 for the unknown item (proves not 401).
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/next?layer=queue", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send_auth(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for uri in [
        "/api/v1/playback/started",
        "/api/v1/playback/ended",
        "/api/v1/playback/error",
    ] {
        let (status, body) = send_auth(
            &app,
            Method::POST,
            uri,
            Some(json!({ "queue_item_id": ghost })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "with token: {uri} {body}");
    }

    // Non-playback routes stay open without any token.
    let (status, _) = send(&app, Method::GET, "/api/v1/queues", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, Method::GET, "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 4. Negatives: invalid layer and unknown queue_item_id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_negative_layer_and_unknown_item() {
    let (_dir, _pool, app) = setup().await;
    let ghost = "ffffffff-ffff-7fff-8fff-ffffffffffff";

    // invalid layer → 422 validation_error with field=layer
    for layer in ["bogus", "rotation", "live", "fun"] {
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

    // unknown queue_item_id → 404 not_found on all three POST routes
    for uri in [
        "/api/v1/playback/started",
        "/api/v1/playback/ended",
        "/api/v1/playback/error",
    ] {
        let (status, body) = send_auth(
            &app,
            Method::POST,
            uri,
            Some(json!({ "queue_item_id": ghost })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "not_found");
    }
}
