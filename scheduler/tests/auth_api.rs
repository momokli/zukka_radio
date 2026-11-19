//! HTTP-level tests for the identity authority API (Issue #16).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets. The `/internal/*`
//! routes are guarded by the service token (§4.1).

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
use zukka_scheduler::store::auth;
use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

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

async fn caster(pool: &SqlitePool, username: &str, password: &str, active: bool) -> String {
    let id = new_id();
    let hash = auth::hash_password(password).expect("hash");
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES (?, ?, 'Caster', ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(&hash)
    .bind(ROLE_CASTER)
    .bind(if active { 1 } else { 0 })
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

/// Sends a request with optional bearer service token and `X-Session-Token`.
async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    with_service_token: bool,
    session_token: Option<&str>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    if with_service_token {
        builder = builder.header("authorization", format!("Bearer {}", service_token()));
    }
    if let Some(token) = session_token {
        builder = builder.header(api_auth::SESSION_TOKEN_HEADER, token);
    }
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value, headers)
}

#[tokio::test]
async fn internal_routes_require_service_token() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    caster(&pool, "mira", "pw", true).await;

    let (status, body, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "mira", "password": "pw" })),
        false,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn verify_status_codes() {
    api_auth::reset_login_limiter();
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    caster(&pool, "mira", "correct", true).await;
    caster(&pool, "off", "correct", false).await;

    // 200
    let (status, body, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "mira", "password": "correct" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["caster"]["username"], "mira");
    assert_eq!(body["caster"]["role"], "caster");
    assert_eq!(body["caster"]["is_active"], true);

    // 401 wrong password
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "mira", "password": "nope" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 401 unknown user (identical code/message)
    let (status, body, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "ghost", "password": "nope" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    // 401 inactive caster
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "off", "password": "correct" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn verify_rate_limit_returns_429() {
    api_auth::reset_login_limiter();
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    caster(&pool, "ratelimited", "correct", true).await;

    for _ in 0..5 {
        let (status, _, _) = send(
            &app,
            Method::POST,
            "/api/v1/internal/auth/verify",
            Some(json!({ "username": "ratelimited", "password": "bad" })),
            true,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "ratelimited", "password": "correct" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "body: {body}");
    assert_eq!(body["error"]["code"], "too_many_requests");
}

#[tokio::test]
async fn session_write_through_and_validation_route() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = caster(&pool, "mira", "pw", true).await;

    // create session → 201, token returned once
    let (status, created, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/sessions",
        Some(json!({ "caster_id": caster_id, "ip_address": "127.0.0.1", "user_agent": "t" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {created}");
    let token = created["token"].as_str().unwrap().to_string();
    let session_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 43);
    assert!(created["csrf_token"].is_string());

    // validate via the read route → 200
    let (status, validation, _) = send(
        &app,
        Method::GET,
        "/api/v1/internal/sessions/current",
        None,
        true,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {validation}");
    assert_eq!(validation["session_id"], session_id);
    assert_eq!(validation["caster"]["id"], caster_id);
    assert_eq!(validation["csrf_token"], created["csrf_token"]);

    // unknown token → 401
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/api/v1/internal/sessions/current",
        None,
        true,
        Some("nope"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // touch → 204
    let (status, _, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/sessions/{session_id}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // revoke → 204, then validation → 401
    let (status, _, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/internal/sessions/{session_id}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/api/v1/internal/sessions/current",
        None,
        true,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_session_for_unknown_caster_is_404() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/sessions",
        Some(json!({ "caster_id": "0192f0a1-0000-7000-8000-00000000dead" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn caster_get_and_password_endpoints() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = caster(&pool, "mira", "login-pw", true).await;

    // GET caster → 200
    let (status, body, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/internal/casters/{caster_id}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "mira");

    // set stream password equal to login password → 422
    let (status, body, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/internal/casters/{caster_id}/stream-password"),
        Some(json!({ "password": "login-pw" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"]["details"]["field"], "stream_password");

    // generate stream password → 200 with a one-time value
    let (status, body, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/internal/casters/{caster_id}/stream-password"),
        Some(json!({})),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["password"].as_str().unwrap().len(), 24);

    // change login password equal to stream password → 422
    let stream_pw = body["password"].as_str().unwrap().to_string();
    let (status, body, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/internal/casters/{caster_id}/password"),
        Some(json!({ "password": stream_pw })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(body["error"]["details"]["field"], "password");
}

#[tokio::test]
async fn unknown_internal_route_is_404_envelope() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let (status, body, _) =
        send(&app, Method::GET, "/api/v1/internal/nope", None, true, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}
