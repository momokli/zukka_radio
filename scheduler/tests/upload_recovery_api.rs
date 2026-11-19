//! HTTP-level restart-safety test for upload jobs (Issue #18, US-10).
//!
//! Complements `upload_jobs_api.rs`: a job is created and driven to `running`
//! **through the real `/internal/upload-jobs` API**, then the startup recovery
//! pass is run and the outcome is read back over HTTP. This proves that a
//! portal upload which died mid-flight never leaves a zombie `running` job
//! (portal.md §7) across the API boundary.

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
use zukka_scheduler::recovery;
use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

fn service_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "recovery-test-token".to_string();
        // SAFETY: set once for the whole test binary, before any request.
        unsafe { std::env::set_var(api_auth::SERVICE_TOKEN_ENV, &token) };
        token
    })
}

async fn setup() -> (TempDir, SqlitePool, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    let caster = new_id();
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES (?, 'r', 'R', 'h', ?, 1, ?, ?)",
    )
    .bind(&caster)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert caster");
    let app = zukka_scheduler::api::router(pool.clone());
    (dir, pool, app)
}

async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", service_token()));
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Creates a job over HTTP and drives it to `running` (attempts = 1).
async fn running_job(app: &Router, caster: &str) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/internal/upload-jobs",
        Some(json!({"caster_id": caster, "filename": "stale.flac"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        app,
        Method::PATCH,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        Some(json!({"state": "running"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "running");
    assert_eq!(body["attempts"], 1);
    id
}

async fn backdate(pool: &SqlitePool, id: &str) {
    sqlx::query("UPDATE upload_jobs SET updated_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .expect("backdate");
}

async fn state_of(app: &Router, id: &str) -> Value {
    let (status, body) = send(
        app,
        Method::GET,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn stale_running_job_is_requeued_over_http() {
    let (_dir, pool, app) = setup().await;
    let caster: String = sqlx::query_scalar("SELECT id FROM casters LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    let id = running_job(&app, &caster).await;
    backdate(&pool, &id).await;

    // 30-min timeout; 1 attempt < 3 → requeue.
    let report = recovery::recover_stale_uploads(&pool, 1800, 3)
        .await
        .unwrap();
    assert_eq!(report.requeued, 1);
    assert_eq!(report.failed, 0);

    let job = state_of(&app, &id).await;
    assert_eq!(job["state"], "pending", "requeued, no zombie running job");
}

#[tokio::test]
async fn stale_running_job_is_failed_after_max_attempts() {
    let (_dir, pool, app) = setup().await;
    let caster: String = sqlx::query_scalar("SELECT id FROM casters LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    let id = running_job(&app, &caster).await;
    // Bump attempts to the ceiling (running -> pending -> running is allowed).
    send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        Some(json!({"state": "pending"})),
    )
    .await;
    send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        Some(json!({"state": "running"})),
    )
    .await;
    backdate(&pool, &id).await;

    let report = recovery::recover_stale_uploads(&pool, 1800, 2)
        .await
        .unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.requeued, 0);

    let job = state_of(&app, &id).await;
    assert_eq!(job["state"], "failed");
    assert_eq!(job["error"], "timeout");
}

#[tokio::test]
async fn fresh_running_job_is_left_alone() {
    let (_dir, pool, app) = setup().await;
    let caster: String = sqlx::query_scalar("SELECT id FROM casters LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();

    // running, but updated_at is "now" → not stale for a 30-min timeout.
    let id = running_job(&app, &caster).await;

    let report = recovery::recover_stale_uploads(&pool, 1800, 3)
        .await
        .unwrap();
    assert_eq!(report, recovery::RecoveryReport::default());

    let job = state_of(&app, &id).await;
    assert_eq!(job["state"], "running", "fresh running job is untouched");
}
