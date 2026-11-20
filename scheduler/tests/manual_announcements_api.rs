//! Integration tests for manual announcements (Issue #38): RBAC + audit log.
//!
//! Like `announcements_api.rs`: `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router`. The viewer context arrives via
//! `X-Caster-Id` (never client input); `announcement.write` is the only
//! guard in the §6.10.1 matrix for this surface.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::announcement::delivery::{DeliveryError, OutboundAnnouncement, Transport};
use zukka_scheduler::announcement::dispatch::run_once;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER_A: &str = "0192aaaa-0001-7000-8000-00000000000a";

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

/// Strips `announcement.write` from the caster role (least privilege).
async fn strip_announcement_write(pool: &SqlitePool) {
    sqlx::query("UPDATE roles SET permissions = '{\"announcement.write\":false}' WHERE id = ?")
        .bind(ROLE_CASTER)
        .execute(pool)
        .await
        .expect("strip permission");
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    caster_id: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(caster_id) = caster_id {
        builder = builder.header("x-caster-id", caster_id);
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

async fn manual(app: &Router, caster_id: Option<&str>) -> (StatusCode, Value) {
    send(
        app,
        Method::POST,
        "/api/v1/announcements",
        caster_id,
        Some(json!({ "event": "manual", "payload": { "message": "hi" } })),
    )
    .await
}

/// Fetches the `control_action` audit rows (via the read-only event log).
async fn control_actions(app: &Router) -> Vec<Value> {
    let (status, body) = send(
        app,
        Method::GET,
        "/api/v1/event-log?event=control_action",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 1: manual POST with viewer → 201 + audit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn manual_post_audits_actor() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, created) = manual(&app, Some(CASTER_A)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let rows = control_actions(&app).await;
    assert_eq!(rows.len(), 1, "exactly one audit row: {rows:?}");
    let row = &rows[0];
    assert_eq!(row["event"], "control_action");
    assert_eq!(row["actor_caster_id"], CASTER_A);
    assert_eq!(row["caster_id"], Value::Null);
    assert_eq!(row["details"]["action"], "announcement_post");
    assert_eq!(row["details"]["announcement_id"], id.as_str());
    assert_eq!(row["details"]["event"], "manual");
    assert_eq!(row["details"]["channel"], "telegram");
}

// ---------------------------------------------------------------------------
// 2: manual POST without header → 401, no audit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn manual_post_without_header_is_401_and_no_audit() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = manual(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "unauthorized");
    assert!(control_actions(&app).await.is_empty());
}

// ---------------------------------------------------------------------------
// 3: custom role without announcement.write → 403, no audit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn manual_post_without_permission_is_403_and_no_audit() {
    let (_dir, pool) = setup().await;
    strip_announcement_write(&pool).await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = manual(&app, Some(CASTER_A)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(control_actions(&app).await.is_empty());
}

// ---------------------------------------------------------------------------
// 4: send with/without header → 200/401 + audit only on 200
// ---------------------------------------------------------------------------

#[tokio::test]
async fn manual_send_audits_actor() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, created) = manual(&app, Some(CASTER_A)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcements/{id}/send"),
        Some(CASTER_A),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "pending");

    // One POST audit + one resend audit.
    let rows = control_actions(&app).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let resend = rows
        .iter()
        .find(|r| r["details"]["action"] == "announcement_resend")
        .expect("resend audit row");
    assert_eq!(resend["actor_caster_id"], CASTER_A);
    assert_eq!(resend["details"]["announcement_id"], id.as_str());
}

#[tokio::test]
async fn manual_send_without_header_is_401_and_no_audit() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (_, created) = manual(&app, Some(CASTER_A)).await;
    let id = created["id"].as_str().unwrap().to_string();
    let before = control_actions(&app).await.len();

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcements/{id}/send"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        control_actions(&app).await.len(),
        before,
        "no new audit row"
    );
}

// ---------------------------------------------------------------------------
// 6: end-to-end — manual post → due → dispatch → audit event_log
// ---------------------------------------------------------------------------

/// Records the number of dispatched sends; every call succeeds.
struct NoopTransport {
    calls: Arc<AtomicUsize>,
}

impl Transport for NoopTransport {
    fn send(
        &self,
        _msg: OutboundAnnouncement,
    ) -> Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn manual_post_dispatches_and_logs_announcement_sent() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());

    // 1) POST /announcements (event=manual) via the RBAC-guarded route.
    let (status, created) = manual(&app, Some(CASTER_A)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    // 2) The manual post is now due for delivery.
    let due = zukka_scheduler::store::announcements::due(&pool, Utc::now())
        .await
        .expect("due");
    assert!(
        due.iter().any(|a| a.id == id),
        "manual announcement is due: {due:?}"
    );

    // 3) One dispatch pass with a fake transport sends it.
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = NoopTransport {
        calls: calls.clone(),
    };
    let report = run_once(&pool, &transport).await.expect("dispatch");
    assert_eq!(report.sent, 1, "one send: {report:?}");
    assert_eq!(report.failed, 0, "no failures: {report:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "transport called once");

    // 4) The audit log additionally carries `announcement_sent`.
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/v1/event-log?event=announcement_sent",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = body.as_array().cloned().unwrap_or_default();
    assert_eq!(sent.len(), 1, "exactly one sent event: {sent:?}");
    assert_eq!(sent[0]["details"]["announcement_id"], id.as_str());

    // The original manual audit row is untouched (same actor).
    let rows = control_actions(&app).await;
    assert_eq!(rows.len(), 1, "one control_action row: {rows:?}");
    assert_eq!(rows[0]["actor_caster_id"], CASTER_A);
    assert_eq!(rows[0]["details"]["action"], "announcement_post");
}

// ---------------------------------------------------------------------------
// 5: GET stays open
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_without_header_is_open() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = send(&app, Method::GET, "/api/v1/announcements", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.is_array());
}
