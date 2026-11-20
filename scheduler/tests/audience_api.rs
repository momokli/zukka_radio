//! HTTP-level tests for `GET /internal/ops/audience` (Issue #42, Deploy-Gate).
//!
//! Runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets. The listener count
//! always falls back to `0` here (the Icecast URL is unreachable), which also
//! exercises the endpoint's fail-open path.

use std::sync::OnceLock;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::api::auth as api_auth;
use zukka_scheduler::api::ops;
use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const FUTURE: &str = "2099-01-01T00:00:00.000Z";

fn service_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "test-service-token".to_string();
        // SAFETY: single-threaded initialisation guarded by OnceLock; the value
        // is only ever set once and never removed.
        unsafe { std::env::set_var(api_auth::SERVICE_TOKEN_ENV, &token) };
        token
    })
}

/// Points the Icecast status URL at a closed port once per process, so the
/// listener count is deterministically the fail-open `0`.
fn unreachable_icecast() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: single-threaded initialisation guarded by OnceLock.
        unsafe { std::env::set_var(ops::ICECAST_STATUS_URL_ENV, "http://127.0.0.1:9/x.xsl") };
    });
}

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    (dir, pool)
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

async fn caster(pool: &SqlitePool) -> String {
    let id = new_id();
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES (?, ?, 'Caster', 'x', ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(format!("caster-{}", &id[..8]))
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

/// Inserts a broadcast row directly (`kind='live'` keeps tape/queue NULL).
async fn broadcast(pool: &SqlitePool, caster_id: &str, status: &str, starts_at: &str) -> String {
    let id = new_id();
    sqlx::query(
        "INSERT INTO broadcasts (id, show_id, caster_id, kind, title, description, starts_at, \
         ends_at, status, tape_id, queue_id, version, created_by, created_at, updated_at) \
         VALUES (?, NULL, ?, 'live', 'Show', '', ?, NULL, ?, NULL, NULL, 1, ?, ?, ?)",
    )
    .bind(&id)
    .bind(caster_id)
    .bind(starts_at)
    .bind(status)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert broadcast");
    id
}

async fn set_live_override(pool: &SqlitePool, caster_id: &str) {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES ('live.override_caster_id', ?, ?)",
    )
    .bind(caster_id)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert setting");
}

async fn audience(pool: SqlitePool, with_token: bool) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/internal/ops/audience");
    if with_token {
        builder = builder.header("authorization", format!("Bearer {}", service_token()));
    }
    let response = app(pool)
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("router response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn audience_requires_service_token() {
    unreachable_icecast();
    let (_dir, pool) = setup().await;

    let (status, body) = audience(pool.clone(), false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], "unauthorized");

    let (status, body) = audience(pool, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
async fn audience_empty_state_is_quiet() {
    unreachable_icecast();
    let (_dir, pool) = setup().await;
    caster(&pool).await;

    let (status, body) = audience(pool, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["listeners"], 0, "body: {body}");
    assert_eq!(body["live_sources"], 0, "body: {body}");
    assert!(body["next_broadcast_start"].is_null(), "body: {body}");
}

#[tokio::test]
async fn audience_counts_running_live_broadcast() {
    unreachable_icecast();
    let (_dir, pool) = setup().await;
    let caster = caster(&pool).await;
    broadcast(&pool, &caster, "live", FUTURE).await;

    let (status, body) = audience(pool, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["live_sources"], 1, "body: {body}");
}

#[tokio::test]
async fn audience_counts_live_override() {
    unreachable_icecast();
    let (_dir, pool) = setup().await;
    let caster = caster(&pool).await;
    set_live_override(&pool, &caster).await;

    let (status, body) = audience(pool, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["live_sources"], 1, "body: {body}");
}

#[tokio::test]
async fn audience_reports_next_scheduled_start() {
    unreachable_icecast();
    let (_dir, pool) = setup().await;
    let caster = caster(&pool).await;
    // A past scheduled broadcast must be ignored …
    broadcast(&pool, &caster, "scheduled", "2000-01-01T00:00:00.000Z").await;
    // … and the earliest future one wins.
    broadcast(&pool, &caster, "scheduled", "2099-06-01T00:00:00.000Z").await;
    broadcast(&pool, &caster, "scheduled", FUTURE).await;

    let (status, body) = audience(pool, true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    // 2099-01-01T00:00:00Z rendered as Berlin winter time.
    assert_eq!(
        body["next_broadcast_start"], "2099-01-01T01:00:00+01:00",
        "body: {body}"
    );
}
