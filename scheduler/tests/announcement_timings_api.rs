//! Integration tests for the announcement-timings API (Issue #36).
//!
//! Like `announcement_templates_api.rs`: requests go through
//! `tower::ServiceExt::oneshot` against `zukka_scheduler::api::router`.

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

/// Creates a show through the API and returns its id.
async fn create_show(app: &Router, slug: &str) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({ "slug": slug, "name": slug, "created_by": CASTER_A })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create show {slug}: {body}");
    body["id"].as_str().unwrap().to_string()
}

async fn get_global(app: &Router) -> Value {
    let (status, body) = send(app, Method::GET, "/api/v1/announcement-timings", None).await;
    assert_eq!(status, StatusCode::OK, "get global: {body}");
    body
}

// ---------------------------------------------------------------------------
// 6: defaults
// ---------------------------------------------------------------------------

#[tokio::test]
async fn global_defaults_without_settings() {
    let (_dir, pool) = setup().await;
    let app = app(pool);

    let body = get_global(&app).await;
    assert_eq!(body["show_reminder_lead_seconds"], 900);
    assert_eq!(body["live_on_delay_seconds"], 0);
    assert_eq!(body["live_off_delay_seconds"], 0);
}

// ---------------------------------------------------------------------------
// 7: partial global patch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_global_is_partial() {
    let (_dir, pool) = setup().await;
    let app = app(pool);

    let (status, body) = send(
        &app,
        Method::PATCH,
        "/api/v1/announcement-timings",
        Some(json!({ "show_reminder_lead_seconds": 600 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {body}");
    assert_eq!(body["show_reminder_lead_seconds"], 600);
    assert_eq!(body["live_on_delay_seconds"], 0);
    assert_eq!(body["live_off_delay_seconds"], 0);

    // Re-read confirms persistence; only the patched field changed.
    let body = get_global(&app).await;
    assert_eq!(body["show_reminder_lead_seconds"], 600);
    assert_eq!(body["live_on_delay_seconds"], 0);
    assert_eq!(body["live_off_delay_seconds"], 0);

    // A second patch leaves the first field intact.
    let (status, body) = send(
        &app,
        Method::PATCH,
        "/api/v1/announcement-timings",
        Some(json!({ "live_on_delay_seconds": 30 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["show_reminder_lead_seconds"], 600);
    assert_eq!(body["live_on_delay_seconds"], 30);
}

// ---------------------------------------------------------------------------
// 8: validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_global_rejects_out_of_range() {
    let (_dir, pool) = setup().await;
    let app = app(pool);

    for (field, value) in [
        ("show_reminder_lead_seconds", json!(-1)),
        ("live_on_delay_seconds", json!(604_801)),
        ("live_off_delay_seconds", json!(-100)),
    ] {
        let (status, body) = send(
            &app,
            Method::PATCH,
            "/api/v1/announcement-timings",
            Some(json!({ field: value })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {body}");
        assert_eq!(body["error"]["code"], "validation_error");
        assert_eq!(body["error"]["details"]["field"], field);
    }

    // The failed patches did not change the stored defaults.
    let body = get_global(&app).await;
    assert_eq!(body["show_reminder_lead_seconds"], 900);
}

// ---------------------------------------------------------------------------
// 9 + 10: per-show overrides
// ---------------------------------------------------------------------------

#[tokio::test]
async fn show_override_wins_and_is_isolated() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let show_a = create_show(&app, "show-a").await;
    let show_b = create_show(&app, "show-b").await;

    // Global 600.
    send(
        &app,
        Method::PATCH,
        "/api/v1/announcement-timings",
        Some(json!({ "show_reminder_lead_seconds": 600 })),
    )
    .await;

    // Show A overrides to 300.
    let (status, body) = send(
        &app,
        Method::PUT,
        &format!("/api/v1/announcement-timings/shows/{show_a}"),
        Some(json!({ "show_reminder_lead_seconds": 300 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put show: {body}");
    assert_eq!(body["show_id"], show_a.as_str());
    assert_eq!(body["effective"]["show_reminder_lead_seconds"], 300);
    assert_eq!(body["effective"]["live_on_delay_seconds"], 0);
    assert_eq!(body["overrides"]["show_reminder_lead_seconds"], 300);
    assert!(body["overrides"]["live_on_delay_seconds"].is_null());

    // GET confirms.
    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcement-timings/shows/{show_a}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["effective"]["show_reminder_lead_seconds"], 300);

    // Show B inherits the global 600.
    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcement-timings/shows/{show_b}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["effective"]["show_reminder_lead_seconds"], 600);
    assert!(body["overrides"]["show_reminder_lead_seconds"].is_null());
}

// ---------------------------------------------------------------------------
// 11: null clears a field; last field clears the row; DELETE is idempotent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn override_null_clearing_and_delete() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());
    let show = create_show(&app, "show-a").await;

    send(
        &app,
        Method::PATCH,
        "/api/v1/announcement-timings",
        Some(json!({ "show_reminder_lead_seconds": 600 })),
    )
    .await;

    // Two overrides set.
    let (status, body) = send(
        &app,
        Method::PUT,
        &format!("/api/v1/announcement-timings/shows/{show}"),
        Some(json!({ "show_reminder_lead_seconds": 300, "live_on_delay_seconds": 60 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["overrides"]["show_reminder_lead_seconds"], 300);
    assert_eq!(body["overrides"]["live_on_delay_seconds"], 60);

    // Clear lead → inherit global 600, row stays for live_on.
    let (status, body) = send(
        &app,
        Method::PUT,
        &format!("/api/v1/announcement-timings/shows/{show}"),
        Some(json!({ "show_reminder_lead_seconds": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["overrides"]["show_reminder_lead_seconds"].is_null());
    assert_eq!(body["effective"]["show_reminder_lead_seconds"], 600);
    assert_eq!(body["overrides"]["live_on_delay_seconds"], 60);

    // Clear the last override → the row is deleted.
    let (status, body) = send(
        &app,
        Method::PUT,
        &format!("/api/v1/announcement-timings/shows/{show}"),
        Some(json!({ "live_on_delay_seconds": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["overrides"]["live_on_delay_seconds"].is_null());

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM show_announcement_timings")
        .fetch_one(&pool)
        .await
        .expect("count overrides");
    assert_eq!(rows, 0, "an all-null override row must be removed");

    // DELETE is idempotent → 204 even when nothing exists.
    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/announcement-timings/shows/{show}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// 12: unknown show → 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_show_is_404() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let uri = format!("/api/v1/announcement-timings/shows/{MISSING_ID}");

    let (status, body) = send(&app, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");

    let (status, _) = send(
        &app,
        Method::PUT,
        &uri,
        Some(json!({ "show_reminder_lead_seconds": 300 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&app, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 8b: per-show validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_show_rejects_out_of_range() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let show = create_show(&app, "show-a").await;

    let (status, body) = send(
        &app,
        Method::PUT,
        &format!("/api/v1/announcement-timings/shows/{show}"),
        Some(json!({ "show_reminder_lead_seconds": 604_801 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["details"]["field"],
        "show_reminder_lead_seconds"
    );
}
