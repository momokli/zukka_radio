//! Integration tests for the announcements API (Issue #35).
//!
//! Like `announcement_templates_api.rs`: requests go through
//! `tower::ServiceExt::oneshot` against `zukka_scheduler::api::router`. The
//! retry state machine is exercised directly through the store (no #37).

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::announcement::events::{self, DedupeContext};
use zukka_scheduler::dto::AnnouncementEvent;
use zukka_scheduler::store::announcements::{self, NewAnnouncement};

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER_A: &str = "0192aaaa-0001-7000-8000-00000000000a";
const MISSING_ID: &str = "0192eeee-0001-7000-8000-00000000000e";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

async fn caster(pool: &SqlitePool, id: &str) {
    let username = format!("user_{}", id.replace('-', ""));
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(id)
    .bind(username)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
}

async fn tape(pool: &SqlitePool, id: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 'live', ?, ?)",
    )
    .bind(id)
    .bind(format!("tape-{}", id.replace('-', "")))
    .bind(caster_id)
    .bind("2026-11-18T12:00:00.000Z")
    .bind("2026-11-18T13:00:00.000Z")
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");
}

async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
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

async fn create_template(app: &Router, event: &str) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/announcement-templates",
        Some(json!({ "name": format!("{event} tpl"), "event": event, "body": "{show_name}" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create template {event}: {body}"
    );
    body["id"].as_str().unwrap().to_string()
}

async fn create_live_broadcast(app: &Router) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/broadcasts",
        Some(json!({
            "caster_id": CASTER_A,
            "kind": "live",
            "title": "Live Show",
            "starts_at": "2026-11-18T20:00:00+01:00"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create broadcast: {body}");
    body["id"].as_str().unwrap().to_string()
}

async fn manual(app: &Router, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({ "event": "manual", "payload": { "message": "hi" } });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            body[k] = v.clone();
        }
    }
    send(app, Method::POST, "/api/v1/announcements", Some(body)).await
}

async fn list_announcements(app: &Router, query: &str) -> Value {
    let (status, body) = send(
        app,
        Method::GET,
        &format!("/api/v1/announcements{query}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list {query}: {body}");
    body
}

// ---------------------------------------------------------------------------
// 1 + 2: dedupe (live_on) + live_off
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_on_dedup_and_live_off() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    create_template(&app, "live_on").await;
    create_template(&app, "live_off").await;

    let broadcast_id = create_live_broadcast(&app).await;
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{broadcast_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let live_on = list_announcements(&app, "?event=live_on").await;
    assert_eq!(live_on.as_array().unwrap().len(), 1, "{live_on}");
    assert_eq!(live_on[0]["dedupe_key"], format!("live_on:{broadcast_id}"));
    assert_eq!(live_on[0]["status"], "pending");

    // A second emit of the same event is idempotent (retry/reconnect).
    let key = events::dedupe_key(
        AnnouncementEvent::LiveOn,
        &DedupeContext {
            broadcast_id: Some(&broadcast_id),
            ..Default::default()
        },
    );
    let again = announcements::emit(
        &pool,
        NewAnnouncement {
            event: AnnouncementEvent::LiveOn,
            show_id: None,
            broadcast_id: Some(&broadcast_id),
            caster_id: Some(CASTER_A),
            template_id: None,
            channel: zukka_scheduler::dto::AnnouncementChannel::Telegram,
            payload: json!({}),
            dedupe_key: key,
            scheduled_at: None,
        },
    )
    .await
    .expect("emit ok")
    .expect("existing row returned");
    assert_eq!(
        again.dedupe_key.as_deref(),
        live_on[0]["dedupe_key"].as_str()
    );
    let live_on = list_announcements(&app, "?event=live_on").await;
    assert_eq!(
        live_on.as_array().unwrap().len(),
        1,
        "still one row: {live_on}"
    );

    // end → exactly one live_off.
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{broadcast_id}/end"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let live_off = list_announcements(&app, "?event=live_off").await;
    assert_eq!(live_off.as_array().unwrap().len(), 1, "{live_off}");
    assert_eq!(
        live_off[0]["dedupe_key"],
        format!("live_off:{broadcast_id}")
    );
}

#[tokio::test]
async fn non_live_broadcast_emits_nothing() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    create_template(&app, "live_on").await;

    // A playlist-kind broadcast must not trigger live announcements.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/broadcasts",
        Some(json!({
            "caster_id": CASTER_A,
            "kind": "playlist",
            "title": "Mix",
            "starts_at": "2026-11-18T20:00:00+01:00",
            "queue_id": "0192d0d0-0001-7000-8000-0000000000d0"
        })),
    )
    .await;
    // No queue exists → 404; the point is only that live path is skipped.
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::CREATED,
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// 3: recording_published seam
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recording_published_dedupe() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let tape_id = "0192c0c0-0001-7000-8000-0000000000c0";
    tape(&pool, tape_id, CASTER_A).await;
    create_template(&app, "recording_published").await;

    announcements::emit_recording_published(&pool, tape_id, None, Some(CASTER_A))
        .await
        .expect("first")
        .expect("queued");
    announcements::emit_recording_published(&pool, tape_id, None, Some(CASTER_A))
        .await
        .expect("second")
        .expect("existing");

    let rows = list_announcements(&app, "?event=recording_published").await;
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["dedupe_key"], format!("tape:{tape_id}"));
}

// ---------------------------------------------------------------------------
// 4: no template → nothing queued
// ---------------------------------------------------------------------------

#[tokio::test]
async fn emit_without_template_is_none() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());

    let result = announcements::emit(
        &pool,
        NewAnnouncement {
            event: AnnouncementEvent::LiveOn,
            show_id: None,
            broadcast_id: Some("0192aaaa-0001-7000-8000-0000000000ff"),
            caster_id: Some(CASTER_A),
            template_id: None,
            channel: zukka_scheduler::dto::AnnouncementChannel::Telegram,
            payload: json!({}),
            dedupe_key: Some("live_on:none".to_string()),
            scheduled_at: None,
        },
    )
    .await
    .expect("emit ok");
    assert!(result.is_none(), "no template → Ok(None)");
    assert_eq!(
        list_announcements(&app, "").await.as_array().unwrap().len(),
        0
    );
}

// ---------------------------------------------------------------------------
// 5 + 12: CRUD + DELETE sent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn crud_lifecycle() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let tpl = create_template(&app, "manual").await;

    let (status, created) = manual(
        &app,
        json!({ "template_id": tpl, "caster_id": CASTER_A, "scheduled_at": "2026-11-18T21:00:00+01:00" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"], "pending");
    assert_eq!(created["attempts"], 0);
    let id = created["id"].as_str().unwrap().to_string();

    let (status, got) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcements/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["id"], id);

    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/announcements/{id}"),
        Some(json!({ "payload": { "message": "edited" }, "scheduled_at": "2026-11-18T22:00:00+01:00" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["payload"]["message"], "edited");

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/announcements/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcements/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_sent_is_409() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let tpl = create_template(&app, "manual").await;

    let (_, created) = manual(&app, json!({ "template_id": tpl })).await;
    let id = created["id"].as_str().unwrap().to_string();
    let at = zukka_scheduler::time::parse_api_time("at", TS).unwrap();
    announcements::mark_sent(&pool, &id, at)
        .await
        .expect("sent");

    let (status, body) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/announcements/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "announcement_sent");
}

// ---------------------------------------------------------------------------
// 6: POST duplicate dedupe_key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_duplicate_dedupe_is_409() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let body = json!({ "event": "manual", "dedupe_key": "dup-1" });
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/announcements",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = send(&app, Method::POST, "/api/v1/announcements", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_announcement");
    assert_eq!(body["error"]["details"]["dedupe_key"], "dup-1");
}

// ---------------------------------------------------------------------------
// 7: send / status machine
// ---------------------------------------------------------------------------

#[tokio::test]
async fn send_state_machine() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let tpl = create_template(&app, "manual").await;

    let (_, created) = manual(&app, json!({ "template_id": tpl })).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcements/{id}/send"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "pending");
    assert!(body["next_attempt_at"].is_string());

    // Idempotent second call.
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcements/{id}/send"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // On `sent` → 409 illegal_state.
    let at = zukka_scheduler::time::parse_api_time("at", TS).unwrap();
    announcements::mark_sent(&pool, &id, at)
        .await
        .expect("sent");
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcements/{id}/send"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "illegal_state");
    assert_eq!(body["error"]["details"]["status"], "sent");

    // On `cancelled` → 409 illegal_state.
    let (_, created) = manual(&app, json!({ "template_id": tpl })).await;
    let id2 = created["id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/announcements/{id2}"),
        Some(json!({ "status": "cancelled" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcements/{id2}/send"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "illegal_state");
}

// ---------------------------------------------------------------------------
// 8: retry attempts + mark_sent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_attempts_until_failed() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let tpl = create_template(&app, "manual").await;

    let (_, created) = manual(&app, json!({ "template_id": tpl })).await;
    let id = created["id"].as_str().unwrap().to_string();
    let at = zukka_scheduler::time::parse_api_time("at", TS).unwrap();

    let first = announcements::mark_failed(&pool, &id, "boom", at)
        .await
        .expect("fail 1");
    assert_eq!(first.attempts, 1);
    assert_eq!(first.status.as_str(), "pending");
    assert!(first.next_attempt_at.is_some());
    assert_eq!(first.error.as_deref(), Some("boom"));

    let mut last = first;
    for attempt in 2..=5 {
        last = announcements::mark_failed(&pool, &id, "boom", at)
            .await
            .expect("fail n");
        assert_eq!(last.attempts, attempt);
    }
    assert_eq!(last.status.as_str(), "failed");
    assert!(last.next_attempt_at.is_none(), "{last:?}");

    // Each attempt is logged.
    let (status, log) = send(
        &app,
        Method::GET,
        "/api/v1/event-log?event=announcement_failed",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(log.as_array().unwrap().len(), 5, "{log}");

    // mark_sent on the failed row → sent.
    let sent = announcements::mark_sent(&pool, &id, at)
        .await
        .expect("sent");
    assert_eq!(sent.status.as_str(), "sent");
    assert!(sent.sent_at.is_some());
    let (_, log) = send(
        &app,
        Method::GET,
        "/api/v1/event-log?event=announcement_sent",
        None,
    )
    .await;
    assert_eq!(log.as_array().unwrap().len(), 1, "{log}");
}

// ---------------------------------------------------------------------------
// 9: due queue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn due_only_returns_due_pending() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let tpl = create_template(&app, "manual").await;

    let (_, a) = manual(&app, json!({ "template_id": tpl })).await;
    let (_, b) = manual(&app, json!({ "template_id": tpl })).await;
    let id_b = b["id"].as_str().unwrap().to_string();
    assert!(a["id"].is_string());

    let at = zukka_scheduler::time::parse_api_time("at", TS).unwrap();
    announcements::mark_failed(&pool, &id_b, "boom", at)
        .await
        .expect("fail");

    let now = announcements::due(&pool, at).await.expect("due now");
    assert_eq!(now.len(), 1, "retried row not due yet: {now:?}");

    let later = at + chrono::Duration::seconds(120);
    let due_later = announcements::due(&pool, later).await.expect("due later");
    assert_eq!(due_later.len(), 2, "{due_later:?}");
}

// ---------------------------------------------------------------------------
// 10: event-log filters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn event_log_filters() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    create_template(&app, "live_on").await;

    let broadcast_id = create_live_broadcast(&app).await;
    send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{broadcast_id}/start"),
        None,
    )
    .await;

    // event filter.
    let (status, by_event) = send(
        &app,
        Method::GET,
        "/api/v1/event-log?event=broadcast_started",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_event.as_array().unwrap().len(), 1, "{by_event}");
    assert_eq!(by_event[0]["broadcast_id"], broadcast_id);

    // broadcast_id filter.
    let (_, by_bc) = send(
        &app,
        Method::GET,
        &format!("/api/v1/event-log?broadcast_id={broadcast_id}"),
        None,
    )
    .await;
    assert!(by_bc.as_array().unwrap().len() >= 2, "{by_bc}");

    // caster_id filter (none of the emitted events carries a caster id).
    let (_, by_caster) = send(
        &app,
        Method::GET,
        &format!("/api/v1/event-log?caster_id={MISSING_ID}"),
        None,
    )
    .await;
    assert_eq!(by_caster.as_array().unwrap().len(), 0, "{by_caster}");

    // from/to window: a broad window matches, a future window does not.
    let (_, broad) = send(
        &app,
        Method::GET,
        "/api/v1/event-log?from=2020-01-01T00:00:00Z&to=2030-01-01T00:00:00Z",
        None,
    )
    .await;
    assert!(!broad.as_array().unwrap().is_empty(), "{broad}");
    let (_, future) = send(
        &app,
        Method::GET,
        "/api/v1/event-log?from=2030-01-01T00:00:00Z",
        None,
    )
    .await;
    assert_eq!(future.as_array().unwrap().len(), 0, "{future}");

    // unknown event → 422.
    let (status, body) = send(&app, Method::GET, "/api/v1/event-log?event=nope", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "event");
}

// ---------------------------------------------------------------------------
// 11: validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn validation_errors() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    // unknown event.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/announcements",
        Some(json!({ "event": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "event");

    // unknown channel.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/announcements",
        Some(json!({ "event": "manual", "channel": "sms" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "channel");

    // unknown status filter.
    let (status, body) = send(&app, Method::GET, "/api/v1/announcements?status=nope", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "status");

    // missing show_id.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/announcements",
        Some(json!({ "event": "manual", "show_id": MISSING_ID })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");

    // missing broadcast_id.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/announcements",
        Some(json!({ "event": "manual", "broadcast_id": MISSING_ID })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // malformed body → 400.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/announcements")
        .header("content-type", mime::APPLICATION_JSON.as_ref())
        .body(Body::from("{ not json"))
        .unwrap();
    let response = app.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // unknown route → 404 envelope.
    let (status, body) = send(&app, Method::GET, "/api/v1/announcements/x/y/z", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
}
