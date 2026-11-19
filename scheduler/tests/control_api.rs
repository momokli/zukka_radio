//! Integration tests for the control API (Issue #15).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets. The control routes are
//! open (#15 carries no token layer), so requests need no header.
//!
//! Matrix T1–T16 from `plan-feat-15-control-api.md` §6.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const CASTER: &str = "c0000000-0000-7000-8000-000000000001";
const TS: &str = "2026-11-19T12:00:00.000Z";

async fn setup() -> (TempDir, SqlitePool, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    let app = zukka_scheduler::api::router(pool.clone());
    (dir, pool, app)
}

/// Sends one request, decoding the JSON body (`Null` when empty).
async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(value) => builder
            .header("content-type", mime::APPLICATION_JSON.as_ref())
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap(),
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

async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    send(app, Method::POST, uri, Some(body)).await
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

async fn seed_setting(pool: &SqlitePool, key: &str, value: &str) {
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
        .bind(key)
        .bind(value)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert setting");
}

async fn seed_queue(pool: &SqlitePool, id: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO queues (id, caster_id, name, description, kind, version, created_at, updated_at) \
         VALUES (?, ?, 'q', '', 'background', 1, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue");
}

async fn seed_playing_item(pool: &SqlitePool, id: &str, queue_id: &str) {
    sqlx::query(
        "INSERT INTO queue_items (id, queue_id, position, kind, link_url, link_state, state, created_at) \
         VALUES (?, ?, 0, 'link', 'https://example.com/x', 'pending', 'playing', ?)",
    )
    .bind(id)
    .bind(queue_id)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue item");
}

async fn seed_broadcast(pool: &SqlitePool, id: &str, caster_id: &str, kind: &str, status: &str) {
    sqlx::query(
        "INSERT INTO broadcasts \
         (id, caster_id, kind, title, description, starts_at, status, version, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, 'B', '', ?, ?, 1, ?, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(kind)
    .bind(TS)
    .bind(status)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert broadcast");
}

async fn setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .expect("fetch setting")
}

async fn item_state(pool: &SqlitePool, id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM queue_items WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("fetch item state")
}

async fn event_count(pool: &SqlitePool, event: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM event_log WHERE event = ?")
        .bind(event)
        .fetch_one(pool)
        .await
        .expect("count events")
}

async fn latest_details(pool: &SqlitePool, event: &str) -> Value {
    let raw: String = sqlx::query_scalar(
        "SELECT details FROM event_log WHERE event = ? ORDER BY occurred_at DESC, id DESC LIMIT 1",
    )
    .bind(event)
    .fetch_one(pool)
    .await
    .expect("fetch details");
    serde_json::from_str(&raw).expect("details json")
}

// ---------------------------------------------------------------------------
// T16: unknown subroute (router fallback)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t16_unknown_subroute() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(&app, "/api/v1/control/nope", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

// ---------------------------------------------------------------------------
// T1–T5: /control/live
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t1_live_set() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    let (status, _) = post(
        &app,
        "/api/v1/control/live",
        json!({ "caster_id": CASTER, "reason": "Caster übernimmt" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        setting(&pool, "live.override_caster_id").await.as_deref(),
        Some(CASTER)
    );
}

#[tokio::test]
async fn t2_live_clear() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_setting(&pool, "live.override_caster_id", CASTER).await;
    let (status, _) = post(
        &app,
        "/api/v1/control/live",
        json!({ "caster_id": null, "reason": "zurück zu FCFS" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(setting(&pool, "live.override_caster_id").await, None);
}

#[tokio::test]
async fn t3_live_unknown_caster() {
    let (_dir, _pool, app) = setup().await;
    let ghost = "ffffffff-ffff-7fff-8fff-ffffffffffff";
    let (status, body) = post(
        &app,
        "/api/v1/control/live",
        json!({ "caster_id": ghost, "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn t4_live_non_uuid() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(
        &app,
        "/api/v1/control/live",
        json!({ "caster_id": "nope", "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "caster_id");
}

#[tokio::test]
async fn t5_live_without_reason() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(&app, "/api/v1/control/live", json!({ "caster_id": null })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "reason");
}

// ---------------------------------------------------------------------------
// T6–T7: /control/switch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t6_switch_valid() {
    let (_dir, pool, app) = setup().await;
    let (status, _) = post(
        &app,
        "/api/v1/control/switch",
        json!({ "layer": "queue", "reason": "Moderations-Ausfall" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        setting(&pool, "control.forced_layer").await.as_deref(),
        Some("queue")
    );
}

#[tokio::test]
async fn t7_switch_unknown_layer() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(
        &app,
        "/api/v1/control/switch",
        json!({ "layer": "x", "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "layer");
}

// ---------------------------------------------------------------------------
// T8–T9: /control/skip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t8_skip_with_playing_item() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_queue(&pool, "a0000000-0000-7000-8000-000000000001", CASTER).await;
    let item = "b0000000-0000-7000-8000-000000000001";
    seed_playing_item(&pool, item, "a0000000-0000-7000-8000-000000000001").await;

    let (status, _) = post(
        &app,
        "/api/v1/control/skip",
        json!({ "reason": "Track gefällt nicht" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(item_state(&pool, item).await, "skipped");
}

#[tokio::test]
async fn t9_skip_without_playing_item() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(&app, "/api/v1/control/skip", json!({ "reason": "x" })).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "no-op with no playing item: {body}"
    );
}

// ---------------------------------------------------------------------------
// T10–T15: /control/pause (+ 409 live guard)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t10_pause_live_not_active() {
    let (_dir, pool, app) = setup().await;
    let (status, _) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": true, "reason": "Pause vor Jingle" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        setting(&pool, "control.paused").await.as_deref(),
        Some("true")
    );
}

#[tokio::test]
async fn t11_pause_live_override_active() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_setting(&pool, "live.override_caster_id", CASTER).await;
    let (status, body) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": true, "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "live_active");
    assert_eq!(body["error"]["details"]["field"], "paused");
    // state untouched
    assert_eq!(setting(&pool, "control.paused").await, None);
}

#[tokio::test]
async fn t12_pause_live_broadcast_active() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_broadcast(
        &pool,
        "e0000000-0000-7000-8000-000000000001",
        CASTER,
        "live",
        "live",
    )
    .await;
    let (status, body) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": true, "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "live_active");
    assert_eq!(body["error"]["details"]["field"], "paused");
}

#[tokio::test]
async fn t13_resume_even_when_live_active() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_setting(&pool, "live.override_caster_id", CASTER).await;
    let (status, _) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": false, "reason": "weiter" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        setting(&pool, "control.paused").await.as_deref(),
        Some("false")
    );
}

#[tokio::test]
async fn t14_pause_non_bool() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": "x", "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "paused");
}

#[tokio::test]
async fn t15_pause_without_reason() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = post(&app, "/api/v1/control/pause", json!({ "paused": true })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "reason");
}

// ---------------------------------------------------------------------------
// T17–T21: E2E gap coverage (event_log persistence, method handling, routing)
// ---------------------------------------------------------------------------

/// The pause flag is not only returned as `204` — it is really persisted in
/// `settings` **and** appended to `event_log` (observable side effects).
#[tokio::test]
async fn t17_pause_persists_state_and_event_log() {
    let (_dir, pool, app) = setup().await;
    let (status, _) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": true, "reason": "Pause vor Jingle" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        setting(&pool, "control.paused").await.as_deref(),
        Some("true")
    );
    assert_eq!(event_count(&pool, "control_action").await, 1);
    assert_eq!(
        latest_details(&pool, "control_action").await["action"],
        "pause"
    );
}

/// `switch` writes `source_switched` with the layer in `source_layer`; `skip`
/// writes a `control_action{action:"skip"}` event.
#[tokio::test]
async fn t18_switch_and_skip_write_event_log() {
    let (_dir, pool, app) = setup().await;
    let (status, _) = post(
        &app,
        "/api/v1/control/switch",
        json!({ "layer": "queue", "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(event_count(&pool, "source_switched").await, 1);
    let layer: String = sqlx::query_scalar(
        "SELECT source_layer FROM event_log WHERE event = 'source_switched' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("source_layer");
    assert_eq!(layer, "queue");

    let (status, _) = post(&app, "/api/v1/control/skip", json!({ "reason": "x" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let details = latest_details(&pool, "control_action").await;
    assert_eq!(details["action"], "skip");
    assert_eq!(details["skipped"], false);
}

/// `live` set/clear each append their own `event_log` event.
#[tokio::test]
async fn t19_live_set_and_clear_write_event_log() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    let (status, _) = post(
        &app,
        "/api/v1/control/live",
        json!({ "caster_id": CASTER, "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(event_count(&pool, "live_override_set").await, 1);

    let (status, _) = post(
        &app,
        "/api/v1/control/live",
        json!({ "caster_id": null, "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(event_count(&pool, "live_override_cleared").await, 1);
}

/// A `GET` on a control route must never be a silent `200`/`204` — it is
/// either `405` (method not allowed) or the `404` JSON envelope, never a crash.
#[tokio::test]
async fn t20_get_methods_not_silently_accepted() {
    let (_dir, _pool, app) = setup().await;
    for path in [
        "/api/v1/control/pause",
        "/api/v1/control/live",
        "/api/v1/control/skip",
        "/api/v1/control/switch",
    ] {
        let (status, body) = send(&app, Method::GET, path, None).await;
        assert!(
            status == StatusCode::METHOD_NOT_ALLOWED
                || (status == StatusCode::NOT_FOUND && body["error"]["code"] == "not_found"),
            "GET {path} -> {status} (body {body})"
        );
        assert!(
            status.is_client_error(),
            "GET {path} unexpectedly succeeded"
        );
    }
    // unknown GET subroute still uses the JSON envelope (§4.6)
    let (status, body) = send(&app, Method::GET, "/api/v1/control/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

// ---------------------------------------------------------------------------
// T22–T26: GET /control/state (Issue #22, US-1)
// ---------------------------------------------------------------------------

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    send(app, Method::GET, uri, None).await
}

/// t20 (adjusted): `GET /control/state` now answers `200` with the state DTO.
#[tokio::test]
async fn t22_state_is_200() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["derived"], true);
    assert_eq!(body["paused"], false);
    assert!(body["forced_layer"].is_null());
    assert!(body["now_playing"].is_null());
    // all six seeded layers, ordered priority DESC, name ASC
    let names: Vec<&str> = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["live", "show", "queue", "rotation", "relay", "fun"]
    );
    assert_eq!(body["sources"][0]["enabled"], true);
    // empty `connected` is a valid acceptance state (O2/K4)
    assert_eq!(body["live"]["connected"], json!([]));
    assert!(body["live"]["override_caster_id"].is_null());
    assert!(body["live"]["active_caster_id"].is_null());
}

/// Settings drive `paused` / `forced_layer`; `forced_layer` is the active layer.
#[tokio::test]
async fn t23_state_reflects_settings() {
    let (_dir, pool, app) = setup().await;
    seed_setting(&pool, "control.paused", "true").await;
    seed_setting(&pool, "control.forced_layer", "queue").await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["paused"], true);
    assert_eq!(body["forced_layer"], "queue");
    assert_eq!(body["active_layer"], "queue");
}

/// A live override sets `active_caster_id` and makes `live` the active layer.
#[tokio::test]
async fn t24_state_live_override() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_setting(&pool, "live.override_caster_id", CASTER).await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["live"]["override_caster_id"], CASTER);
    assert_eq!(body["live"]["active_caster_id"], CASTER);
    assert_eq!(body["active_layer"], "live");
}

/// Connected casters are derived from `event_log` (K4): a `live_connected`
/// without a later `live_disconnected`; a disconnect removes the caster.
#[tokio::test]
async fn t25_state_connected_from_event_log() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    sqlx::query(
        "INSERT INTO event_log (id, occurred_at, event, caster_id, source_layer, details, created_at) \
         VALUES (?, '2026-11-19T12:00:00.000Z', 'live_connected', ?, NULL, '{}', ?)",
    )
    .bind("f0000000-0000-7000-8000-000000000001")
    .bind(CASTER)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert connected");
    // a non-live event must not appear
    sqlx::query(
        "INSERT INTO event_log (id, occurred_at, event, caster_id, source_layer, details, created_at) \
         VALUES (?, ?, 'control_action', NULL, NULL, '{}', ?)",
    )
    .bind("f0000000-0000-7000-8000-000000000002")
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert control_action");

    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let connected = body["live"]["connected"].as_array().unwrap();
    assert_eq!(connected.len(), 1);
    assert_eq!(connected[0]["caster_id"], CASTER);
    assert_eq!(connected[0]["since"], "2026-11-19T12:00:00.000Z");

    // a later disconnect removes the caster again
    sqlx::query(
        "INSERT INTO event_log (id, occurred_at, event, caster_id, source_layer, details, created_at) \
         VALUES (?, '2026-11-19T13:00:00.000Z', 'live_disconnected', ?, NULL, '{}', ?)",
    )
    .bind("f0000000-0000-7000-8000-000000000003")
    .bind(CASTER)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert disconnected");
    let (_, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(body["live"]["connected"], json!([]));
}

/// An active queue with a pending item derives the `queue` layer.
#[tokio::test]
async fn t26_state_active_queue_layer() {
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    let queue_id = "a0000000-0000-7000-8000-000000000009";
    seed_queue(&pool, queue_id, CASTER).await;
    seed_setting(&pool, "queue.active_id", queue_id).await;
    sqlx::query(
        "INSERT INTO tracks (id, music_api_id, title, duration_ms, storage_key, visibility, created_at, updated_at) \
         VALUES ('t-1', 'm-1', 'Track', 0, 'k', 'shared', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert track");
    sqlx::query(
        "INSERT INTO queue_items (id, queue_id, position, kind, track_id, state, created_at) \
         VALUES ('b0000000-0000-7000-8000-000000000009', ?, 0, 'track', 't-1', 'pending', ?)",
    )
    .bind(queue_id)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert pending item");
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active_layer"], "queue");
}

/// The `/control` nest must not shadow `/playback`: the token-guarded playback
/// router is still mounted (401 without token), and control stays reachable.
#[tokio::test]
async fn t21_control_does_not_collide_with_playback() {
    let (_dir, _pool, app) = setup().await;
    let (status, body) = send(&app, Method::GET, "/api/v1/playback/current", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "playback route intact: {body}"
    );
    let (status, _) = post(
        &app,
        "/api/v1/control/pause",
        json!({ "paused": false, "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
