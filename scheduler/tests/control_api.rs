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

/// Serializes tests: `ICECAST_STATUS_URL` is process-global.
fn state_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Serializes state tests and points the observation at a dead port so results
/// are deterministic (no real Icecast on the test host).
async fn lock_state_env() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = state_env_lock().lock().await;
    // Rust 2024: process-env mutation is unsafe; `STATE_ENV_LOCK` serializes it.
    unsafe {
        std::env::set_var(
            zukka_scheduler::icecast::ICECAST_STATUS_URL_ENV,
            "http://127.0.0.1:1/status-json.xsl",
        );
    }
    guard
}

/// Minimal one-shot HTTP server serving a fixed JSON body; returns its URL.
async fn spawn_icecast_fixture(body: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture server");
    let addr = listener.local_addr().expect("fixture addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/status-json.xsl")
}

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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_control_env().await;
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
    let _env = lock_state_env().await;
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
    let _env = lock_state_env().await;
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
    let _env = lock_state_env().await;
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
    let _env = lock_state_env().await;
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
    let _env = lock_state_env().await;
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
    let _env = lock_control_env().await;
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

// ---------------------------------------------------------------------------
// T27–T28: observed layer from Icecast (Issue #140)
// ---------------------------------------------------------------------------

/// T27: a reachable Icecast fixture with external ICY metadata makes the
/// observed `relay` layer effective and exposes the observed now-playing.
#[tokio::test]
async fn t27_state_observed_relay_from_icecast() {
    let _env = lock_state_env().await;
    let url = spawn_icecast_fixture(
        r#"{"icestats":{"source":{"server_name":"zukka","listenurl":"http://icecast:8000/stream.mp3",
        "title":"SomaFM - Deep Space","artist":"SomaFM","listeners":9,
        "stream_start":"2026-11-20T10:00:00Z"}}}"#,
    )
    .await;
    // Rust 2024: process-env mutation is unsafe; `STATE_ENV_LOCK` serializes it.
    unsafe {
        std::env::set_var(zukka_scheduler::icecast::ICECAST_STATUS_URL_ENV, &url);
    }

    let (_dir, _pool, app) = setup().await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["observed_layer"], "relay");
    assert_eq!(body["derived_layer"], Value::Null);
    assert_eq!(body["active_layer"], "relay");
    assert_eq!(body["active_layer_source"], "observed");
    assert_eq!(body["observed"], true);
    assert_eq!(body["observed_reason"], "external");
    assert_eq!(body["on_air"], true);
    assert_eq!(body["observed_now_playing"]["title"], "SomaFM - Deep Space");
    assert_eq!(body["observed_now_playing"]["artist"], "SomaFM");
    assert_eq!(body["derived"], true, "compat marker unchanged");
}

/// T28: an unreachable Icecast keeps `/control/state` a `200`, marks
/// `observed=false` and falls back to the DB-derived layer.
#[tokio::test]
async fn t28_state_unreachable_icecast_falls_back_to_derived() {
    let _env = lock_state_env().await;
    let (_dir, pool, app) = setup().await;
    seed_setting(&pool, "control.forced_layer", "queue").await;

    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "never 5xx: {body}");
    assert_eq!(body["observed_layer"], Value::Null);
    assert_eq!(body["derived_layer"], "queue");
    assert_eq!(body["active_layer"], "queue");
    assert_eq!(body["active_layer_source"], "derived");
    assert_eq!(body["observed"], false);
    assert_eq!(body["observed_reason"], "icecast_unreachable");
    assert_eq!(body["on_air"], false);
    assert!(body["observed_now_playing"].is_null());
}

/// T27b: a silent Icecast (no source) is `on_air=false` and never invents a
/// layer; the DB derivation still applies.
#[tokio::test]
async fn t27_silent_icecast_keeps_derived_layer() {
    let _env = lock_state_env().await;
    let url = spawn_icecast_fixture(r#"{"icestats":{"admin":"zukka"}}"#).await;
    // Rust 2024: process-env mutation is unsafe; `STATE_ENV_LOCK` serializes it.
    unsafe {
        std::env::set_var(zukka_scheduler::icecast::ICECAST_STATUS_URL_ENV, &url);
    }

    let (_dir, pool, app) = setup().await;
    seed_setting(&pool, "control.relay_active", "true").await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["on_air"], false);
    assert_eq!(body["observed_reason"], "silent");
    assert_eq!(body["observed_layer"], Value::Null);
    // relay setting still derives `relay` on the DB path.
    assert_eq!(body["active_layer"], "relay");
    assert_eq!(body["active_layer_source"], "derived");
}

/// DD7 (#140): `zukka_source_state` stays on the derived K3 layer even when
/// Icecast observes an external relay; the observed layer is reported only via
/// `zukka_source_observed`.
#[tokio::test]
async fn t27_metrics_source_state_stays_derived() {
    let _env = lock_state_env().await;
    let url = spawn_icecast_fixture(
        r#"{"icestats":{"source":{"server_name":"zukka","listenurl":"http://icecast:8000/stream.mp3",
        "title":"SomaFM - Deep Space","artist":"SomaFM","listeners":9}}}"#,
    )
    .await;
    // Rust 2024: process-env mutation is unsafe; `STATE_ENV_LOCK` serializes it.
    unsafe {
        std::env::set_var(zukka_scheduler::icecast::ICECAST_STATUS_URL_ENV, &url);
    }

    let (_dir, pool, app) = setup().await;
    seed_setting(&pool, "control.forced_layer", "queue").await;

    let request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("metrics response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes);

    // Derived (K3) layer stays authoritative for the monitoring contract...
    assert!(
        body.contains(r#"zukka_source_state{layer="queue"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"zukka_source_state{layer="relay"} 0"#),
        "{body}"
    );
    // ...while the observed relay is exposed separately.
    assert!(
        body.contains(r#"zukka_source_observed{layer="relay"} 1"#),
        "{body}"
    );
    assert!(
        body.contains(r#"zukka_source_observed{layer="queue"} 0"#),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// Issue #147: control plan + command proxy tests (T29+)
// ---------------------------------------------------------------------------

/// Serializes **all** control env mutation on the shared lock and points the
/// Icecast observation at a dead port (deterministic derived path).
async fn lock_control_env() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = state_env_lock().lock().await;
    unsafe {
        std::env::set_var(
            zukka_scheduler::icecast::ICECAST_STATUS_URL_ENV,
            "http://127.0.0.1:1/status-json.xsl",
        );
    }
    guard
}

/// Sets/unsets `ZUKKA_LIQUIDSOAP_CONTROL_URL`, restoring the previous value on
/// drop so tests stay hermetic even on panic.
struct ControlUrlGuard {
    previous: Option<std::ffi::OsString>,
}

impl ControlUrlGuard {
    fn set(url: Option<&str>) -> Self {
        let key = zukka_scheduler::liquidsoap::CONTROL_URL_ENV;
        let previous = std::env::var_os(key);
        unsafe {
            match url {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        ControlUrlGuard { previous }
    }
}

impl Drop for ControlUrlGuard {
    fn drop(&mut self) {
        let key = zukka_scheduler::liquidsoap::CONTROL_URL_ENV;
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Minimal engine fixture: records each received command body and answers every
/// request with a fixed response body.
async fn spawn_engine_fixture(
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind engine fixture");
    let addr = listener.local_addr().expect("engine fixture addr");
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log2 = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let log = log2.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).to_string();
                if let Some((_, request_body)) = request.split_once("\r\n\r\n") {
                    log.lock().unwrap().push(request_body.trim().to_string());
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}/control"), log)
}

fn received(log: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().unwrap().clone()
}

// --- P: GET/PUT /control/plan --------------------------------------------

#[tokio::test]
async fn t29_p1_get_plan_empty_db() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = get(&app, "/api/v1/control/plan").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["preset"].is_null());
    assert!(body["forced_layer"].is_null());
    assert_eq!(body["paused"], false);
}

#[tokio::test]
async fn t30_p2_put_plan_preset_pushes_engine() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;

    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "preset": "live", "reason": "op" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["preset"], "live");
    assert_eq!(body["engine"]["preset"], "applied");
    assert_eq!(
        setting(&pool, "control.preset").await.as_deref(),
        Some("live")
    );
    assert_eq!(event_count(&pool, "control_action").await, 1);
    assert!(received(&log).iter().any(|c| c == "layer.preset live"));
}

#[tokio::test]
async fn t31_p3_put_plan_unknown_preset() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "preset": "rotation", "reason": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "preset");
}

#[tokio::test]
async fn t32_p4_put_plan_unknown_layer() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "forced_layer": "x", "reason": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "forced_layer");
}

#[tokio::test]
async fn t33_p5_put_plan_non_bool_paused() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "paused": "x", "reason": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "paused");
}

#[tokio::test]
async fn t34_p6_put_plan_without_reason() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "preset": "live" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "reason");
}

#[tokio::test]
async fn t35_p7_put_plan_pause_live_active() {
    let _env = lock_control_env().await;
    let (_dir, pool, app) = setup().await;
    seed_caster(&pool, CASTER).await;
    seed_setting(&pool, "live.override_caster_id", CASTER).await;
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "paused": true, "reason": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "live_active");
    assert_eq!(body["error"]["details"]["field"], "paused");
    assert_eq!(setting(&pool, "control.paused").await, None);
}

#[tokio::test]
async fn t36_p8_put_plan_null_deletes_keys() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;
    seed_setting(&pool, "control.preset", "live").await;
    seed_setting(&pool, "control.forced_layer", "queue").await;

    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "preset": null, "forced_layer": null, "reason": "clear" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(setting(&pool, "control.preset").await, None);
    assert_eq!(setting(&pool, "control.forced_layer").await, None);
    assert!(received(&log).iter().any(|c| c == "layer.force none"));
    // P8: deleting the preset resets the engine to the documented default.
    assert!(received(&log).iter().any(|c| c == "layer.preset default"));
    assert_eq!(body["engine"]["preset"], "applied");
    assert_eq!(body["engine"]["forced_layer"], "applied");
}

#[tokio::test]
async fn t37_p9_put_plan_engine_down_still_200() {
    let _env = lock_control_env().await;
    let _url = ControlUrlGuard::set(Some("http://127.0.0.1:1/control"));
    let (_dir, pool, app) = setup().await;
    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({ "forced_layer": "queue", "reason": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["forced_layer"], "queue");
    assert_eq!(body["engine"]["forced_layer"], "failed");
    assert_eq!(
        setting(&pool, "control.forced_layer").await.as_deref(),
        Some("queue")
    );
}

// --- C: POST /control/command (proxy) ------------------------------------

async fn proxy(app: &Router, cmd: &str, args: Value, reason: &str) -> (StatusCode, Value) {
    post(
        app,
        "/api/v1/control/command",
        json!({ "cmd": cmd, "args": args, "reason": reason }),
    )
    .await
}

#[tokio::test]
async fn t38_c1_proxy_mixer_set_ok() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;

    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0 }),
        "Fader",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cmd"], "mixer.set");
    assert_eq!(body["result"]["applied"], true);
    assert_eq!(body["raw"], "OK");
    assert_eq!(event_count(&pool, "control_action").await, 1);
    let details = latest_details(&pool, "control_action").await;
    assert_eq!(details["action"], "command");
    assert_eq!(details["outcome"], "ok");
    assert!(received(&log).iter().any(|c| c == "mixer.set live -6"));
}

#[tokio::test]
async fn t39_c2_proxy_unknown_command() {
    let _env = lock_control_env().await;
    let (_dir, pool, app) = setup().await;
    let (status, body) = proxy(&app, "frobnicate", json!({}), "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "unknown_command");
    assert_eq!(event_count(&pool, "control_action").await, 1);
    assert_eq!(
        latest_details(&pool, "control_action").await["cmd"],
        "frobnicate"
    );
}

#[tokio::test]
async fn t40_c3_proxy_mixer_set_missing_channel() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = proxy(&app, "mixer.set", json!({ "gain_db": -6.0 }), "x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_args");
    assert_eq!(body["error"]["details"]["field"], "channel");
}

#[tokio::test]
async fn t41_c4_c5_proxy_bad_gain() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": "abc" }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_gain");

    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -99.0 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_gain");
}

#[tokio::test]
async fn t42_c6_proxy_gain_db_and_linear() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0, "linear": 0.5 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_args");
}

#[tokio::test]
async fn t43_c7_proxy_bad_source() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.patch",
        json!({ "channel": "live", "source": "ftp:x" }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_source");
}

#[tokio::test]
async fn t44_c8_proxy_slot_channel_engine_unknown() {
    let _env = lock_control_env().await;
    let (url, _log) = spawn_engine_fixture("ERROR unknown_channel: adhoc-1").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, _pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "adhoc-1", "linear": 0.5 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "channel_not_found");
}

#[tokio::test]
async fn t45_c9_c10_proxy_engine_errors() {
    let _env = lock_control_env().await;
    // C9: engine bad_gain → 422 bad_gain
    let (url, _log) = spawn_engine_fixture("ERROR bad_gain: -99").await;
    let guard = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_gain");
    assert_eq!(
        latest_details(&pool, "control_action").await["outcome"],
        "bad_gain"
    );
    drop(guard);

    // C10: unmapped engine error → 502 command_failed
    let (url, _log) = spawn_engine_fixture("ERROR we-failed").await;
    let _guard = ControlUrlGuard::set(Some(&url));
    let (_dir, _pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "command_failed");
}

#[tokio::test]
async fn t46_c11_c12_proxy_engine_unavailable_audited() {
    let _env = lock_control_env().await;
    let _url = ControlUrlGuard::set(None);
    let (_dir, pool, app) = setup().await;
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "engine_unavailable");
    assert_eq!(event_count(&pool, "control_action").await, 1);
    let details = latest_details(&pool, "control_action").await;
    assert_eq!(details["cmd"], "mixer.set");
    assert_eq!(details["outcome"], "engine_unavailable");
}

// --- S: GET /control/state projection ------------------------------------

/// A six-entry `mixer.meters` fixture body.
const METERS_JSON: &str = r#"[
 {"id":"live","rms_l":-18.2,"rms_r":-18.0,"peak":-3.1,"limiter_active":false},
 {"id":"show","rms_l":-32.0,"rms_r":-32.4,"peak":-12.0,"limiter_active":false},
 {"id":"queue","rms_l":-40.0,"rms_r":-40.1,"peak":-20.0,"limiter_active":false},
 {"id":"rotation","rms_l":-22.0,"rms_r":-22.0,"peak":-6.0,"limiter_active":false},
 {"id":"relay","rms_l":-50.0,"rms_r":-50.0,"peak":-30.0,"limiter_active":false},
 {"id":"fun","rms_l":-60.0,"rms_r":-60.0,"peak":-40.0,"limiter_active":false}
]"#;

#[tokio::test]
async fn t47_s1_state_engine_down_channels_null() {
    let _env = lock_control_env().await;
    let _url = ControlUrlGuard::set(None);
    let (_dir, _pool, app) = setup().await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let channels = body["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 6);
    for channel in channels {
        assert!(channel["rms_l"].is_null());
        assert!(channel["peak"].is_null());
    }
}

#[tokio::test]
async fn t48_s2_state_meters_from_fixture() {
    let _env = lock_control_env().await;
    let (url, _log) = spawn_engine_fixture(METERS_JSON).await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, _pool, app) = setup().await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["live", "show", "queue", "rotation", "relay", "fun"]
    );
    assert_eq!(body["channels"][0]["rms_l"], -18.2);
    assert_eq!(body["channels"][3]["peak"], -6.0);
}

#[tokio::test]
async fn t49_s3_s5_state_automation_projection() {
    let _env = lock_control_env().await;
    let _url = ControlUrlGuard::set(None);
    let (_dir, pool, app) = setup().await;

    // S5: empty DB → auto mode, master defaults, six channels.
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["automation"]["mode"], "auto");
    assert!(body["automation"]["override"].is_null());
    assert!(body["master"]["gain_db"].is_null());
    assert_eq!(body["master"]["outputs"]["icecast"], true);
    assert_eq!(body["channels"].as_array().unwrap().len(), 6);

    // S3: forced_layer + preset → manual + preset from plan.
    seed_setting(&pool, "control.forced_layer", "queue").await;
    seed_setting(&pool, "control.preset", "default").await;
    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["automation"]["mode"], "manual");
    assert_eq!(body["automation"]["preset"], "default");
    assert_eq!(body["automation"]["override"]["channel"], "queue");
}

#[tokio::test]
async fn t50_s4_state_140_fields_unchanged_and_shape() {
    let _env = lock_control_env().await;
    let _url = ControlUrlGuard::set(None);
    let (_dir, pool, app) = setup().await;
    seed_setting(&pool, "control.forced_layer", "queue").await;

    let (status, body) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // K3: legacy #22 fields still present.
    assert_eq!(body["derived"], true);
    assert_eq!(body["forced_layer"], "queue");
    assert_eq!(body["active_layer"], "queue");
    assert!(body["sources"].is_array());
    assert!(body["live"].is_object());
    // S4: #140 fields present and consistent (no Icecast observable here).
    assert!(body["observed_layer"].is_null());
    assert_eq!(body["derived_layer"], "queue");
    assert_eq!(body["active_layer_source"], "derived");
    assert_eq!(body["observed"], false);
    assert_eq!(body["on_air"], false);
}

// --- R: boot reconcile ----------------------------------------------------

#[tokio::test]
async fn t51_r1_r3_reconcile_replays_plan() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, _app) = setup().await;
    seed_setting(&pool, "control.preset", "live").await;
    seed_setting(&pool, "control.forced_layer", "queue").await;

    zukka_scheduler::store::control::reconcile(&pool)
        .await
        .expect("reconcile ok");
    let first = received(&log);
    assert_eq!(
        first.iter().filter(|c| *c == "layer.preset live").count(),
        1
    );
    assert_eq!(
        first.iter().filter(|c| *c == "layer.force queue").count(),
        1
    );

    // R3: idempotent replay — same end state, no error.
    zukka_scheduler::store::control::reconcile(&pool)
        .await
        .expect("reconcile ok again");
    let second = received(&log);
    assert_eq!(
        second.iter().filter(|c| *c == "layer.preset live").count(),
        2
    );
    assert_eq!(
        second.iter().filter(|c| *c == "layer.force queue").count(),
        2
    );
    assert_eq!(
        setting(&pool, "control.preset").await.as_deref(),
        Some("live")
    );
    assert_eq!(
        setting(&pool, "control.forced_layer").await.as_deref(),
        Some("queue")
    );
}

#[tokio::test]
async fn t52_r2_reconcile_engine_down_fails_open() {
    let _env = lock_control_env().await;
    let _url = ControlUrlGuard::set(Some("http://127.0.0.1:1/control"));
    let (_dir, pool, _app) = setup().await;
    seed_setting(&pool, "control.preset", "live").await;
    seed_setting(&pool, "control.forced_layer", "queue").await;
    // Never fails: an unreachable engine is only a warning.
    zukka_scheduler::store::control::reconcile(&pool)
        .await
        .expect("reconcile must not fail when the engine is down");
}

#[tokio::test]
async fn t53_switch_pushes_layer_force() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;
    let (status, _) = post(
        &app,
        "/api/v1/control/switch",
        json!({ "layer": "queue", "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        setting(&pool, "control.forced_layer").await.as_deref(),
        Some("queue")
    );
    assert!(received(&log).iter().any(|c| c == "layer.force queue"));
}

#[tokio::test]
async fn t54_r4_reconcile_without_preset_pushes_default() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, _app) = setup().await;
    // No `control.preset` key persisted: the engine default is the documented
    // start distribution, so reconcile must re-assert it.
    zukka_scheduler::store::control::reconcile(&pool)
        .await
        .expect("reconcile ok");
    assert!(
        received(&log).iter().any(|c| c == "layer.preset default"),
        "reconcile must push the default preset when no key is set"
    );
}

// --- C13/C14/C15: proxy hardening (rework #147) ---------------------------

#[tokio::test]
async fn t55_c13_proxy_monitor_enable_unknown_command() {
    let _env = lock_control_env().await;
    let (_dir, pool, app) = setup().await;
    let (status, body) = proxy(&app, "monitor.enable", json!({ "enabled": true }), "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "unknown_command");
    assert_eq!(event_count(&pool, "control_action").await, 1);
    assert_eq!(
        latest_details(&pool, "control_action").await["cmd"],
        "monitor.enable"
    );
}

#[tokio::test]
async fn t56_c14_proxy_explicit_null_is_absent() {
    let _env = lock_control_env().await;
    let (_dir, _pool, app) = setup().await;

    // `gain_db: null` behaves like an absent field ⇒ invalid_args (not bad_gain).
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": null }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_args");

    // `linear: null` likewise.
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "linear": null }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_args");

    // A missing (or null) required gain is a missing argument, not a range error.
    let (status, body) = proxy(
        &app,
        "mixer.trim",
        json!({ "channel": "live", "gain_db": null }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_args");

    let (status, body) = proxy(&app, "mixer.master", json!({ "gain_db": null }), "x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_args");
}

#[tokio::test]
async fn t57_c15_proxy_audit_args_bounded() {
    let _env = lock_control_env().await;
    let (url, _log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;
    let blob = "x".repeat(2000);
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0, "blob": blob }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let details = latest_details(&pool, "control_action").await;
    let args = details["args"]
        .as_str()
        .expect("args is a serialized string");
    assert_eq!(args.chars().count(), 1024);
}

// --- Tester-stage additions (integrative gaps #147) -----------------------

/// Roundtrip: `PUT /plan` (preset + forced_layer + paused) → `GET /plan`
/// identical; `GET /state` folds the plan into `automation`/`override` while the
/// `#140` fields stay untouched.
#[tokio::test]
async fn t58_plan_roundtrip_and_state_projection() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, _pool, app) = setup().await;

    let (status, put_body) = send(
        &app,
        Method::PUT,
        "/api/v1/control/plan",
        Some(json!({
            "preset": "live",
            "forced_layer": "queue",
            "paused": false,
            "reason": "op"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{put_body}");

    // GET must echo the persisted plan identically.
    let (status, get_body) = get(&app, "/api/v1/control/plan").await;
    assert_eq!(status, StatusCode::OK, "{get_body}");
    assert_eq!(get_body["preset"], "live");
    assert_eq!(get_body["forced_layer"], "queue");
    assert_eq!(get_body["paused"], false);
    assert_eq!(get_body["preset"], put_body["preset"]);
    assert_eq!(get_body["forced_layer"], put_body["forced_layer"]);
    assert_eq!(get_body["paused"], put_body["paused"]);

    // Each set key reached the engine exactly once.
    let reqs = received(&log);
    assert_eq!(reqs.iter().filter(|c| *c == "layer.preset live").count(), 1);
    assert_eq!(reqs.iter().filter(|c| *c == "layer.force queue").count(), 1);

    // State folds the plan into automation/override; #140 fields unchanged.
    let (status, state) = get(&app, "/api/v1/control/state").await;
    assert_eq!(status, StatusCode::OK, "{state}");
    assert_eq!(state["automation"]["mode"], "manual");
    assert_eq!(state["automation"]["preset"], "live");
    assert_eq!(state["automation"]["override"]["channel"], "queue");
    assert_eq!(state["derived_layer"], "queue");
    assert_eq!(state["active_layer"], "queue");
    assert!(state["observed_layer"].is_null());
    assert_eq!(state["observed"], false);
    assert_eq!(state["on_air"], false);
}

/// Proxy request counting: a successful push reaches the engine exactly once and
/// a scheduler-side validation error must **not** hit the engine at all — while
/// both still produce exactly one audit row each.
#[tokio::test]
async fn t59_proxy_engine_request_counting() {
    let _env = lock_control_env().await;
    let (url, log) = spawn_engine_fixture("OK").await;
    let _url = ControlUrlGuard::set(Some(&url));
    let (_dir, pool, app) = setup().await;

    // Success path: exactly one engine request, one audit row.
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -6.0 }),
        "Fader",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(received(&log).len(), 1, "success must push exactly once");
    assert_eq!(event_count(&pool, "control_action").await, 1);

    // Validation error (gain range): engine must stay untouched.
    let (status, body) = proxy(
        &app,
        "mixer.set",
        json!({ "channel": "live", "gain_db": -99.0 }),
        "x",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "bad_gain");
    assert_eq!(
        received(&log).len(),
        1,
        "validation errors must not reach the engine"
    );
    assert_eq!(event_count(&pool, "control_action").await, 2);
    assert_eq!(
        latest_details(&pool, "control_action").await["outcome"],
        "bad_gain"
    );
}
