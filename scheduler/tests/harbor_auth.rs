//! HTTP-level tests for `POST /internal/harbor/auth` (Issues #33 + #105).
//!
//! Runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets. Since Issue #105 the
//! route is Liquidsoap-facing and requires the Liquidsoap token
//! (`X-Liquidsoap-Token` = `$LIQUIDSOAP_TOKEN`), **not** the portal service
//! token (§4.1).

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

/// The Liquidsoap token header (§4.1) used by the route's token layer.
const TOKEN_HEADER: &str = "x-liquidsoap-token";

fn liquidsoap_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "test-liquidsoap-token".to_string();
        // SAFETY: single-threaded initialisation guarded by OnceLock; the value
        // is only ever set once and never removed.
        unsafe { std::env::set_var(api_auth::TOKEN_ENV, &token) };
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

/// Inserts a caster with stream credentials. `stream` = `None` omits the
/// stream hash (unprovisioned caster). Returns the caster id.
async fn caster(
    pool: &SqlitePool,
    username: &str,
    display_name: &str,
    stream: Option<(&str, &str)>,
    mount: Option<&str>,
    active: bool,
) -> String {
    let id = new_id();
    let login_hash = auth::hash_password("login-pw").expect("hash");
    let stream_username = stream.map(|(u, _)| u);
    let stream_hash = stream.map(|(_, p)| auth::hash_password(p).expect("hash"));
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, \
         stream_username, stream_password_hash, harbor_mount, is_active, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(display_name)
    .bind(&login_hash)
    .bind(ROLE_CASTER)
    .bind(stream_username)
    .bind(stream_hash)
    .bind(mount)
    .bind(if active { 1 } else { 0 })
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

/// POSTs `body` to the route with an optional `X-Liquidsoap-Token` header.
async fn send(app: &Router, body: Value, with_liquidsoap_token: bool) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/internal/harbor/auth")
        .header("content-type", "application/json");
    if with_liquidsoap_token {
        builder = builder.header(TOKEN_HEADER, liquidsoap_token());
    }
    let request = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// POSTs `body` with explicit headers, returning the status only.
async fn send_raw(app: &Router, body: Value, headers: &[(&str, &str)]) -> StatusCode {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/internal/harbor/auth")
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    app.clone()
        .oneshot(request)
        .await
        .expect("router response")
        .status()
}

#[tokio::test]
async fn accepts_valid_stream_credentials() {
    let (_dir, pool) = setup().await;
    let id = caster(
        &pool,
        "mira",
        "Mira",
        Some(("mira-stream", "s3cret")),
        Some("/live/slot-01"),
        true,
    )
    .await;

    let (status, body) = send(
        &app(pool),
        json!({ "mount": "/live/slot-01", "username": "mira-stream", "password": "s3cret" }),
        true,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["caster_id"], id);
    assert_eq!(body["display_name"], "Mira");
    assert_eq!(body["mount"], "/live/slot-01");
}

#[tokio::test]
async fn stream_username_is_case_insensitive() {
    let (_dir, pool) = setup().await;
    caster(
        &pool,
        "mira",
        "Mira",
        Some(("MiraStream", "s3cret")),
        Some("/live/slot-01"),
        true,
    )
    .await;

    let (status, _) = send(
        &app(pool),
        json!({ "mount": "/live/slot-01", "username": "mirastream", "password": "s3cret" }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn rejects_everything_else_fail_closed() {
    let (_dir, pool) = setup().await;
    caster(
        &pool,
        "mira",
        "Mira",
        Some(("mira-stream", "s3cret")),
        Some("/live/slot-01"),
        true,
    )
    .await;
    // inactive caster on slot 02
    caster(
        &pool,
        "off",
        "Off",
        Some(("off-stream", "s3cret")),
        Some("/live/slot-02"),
        false,
    )
    .await;
    // active caster without stream credentials on slot 03
    caster(&pool, "nopw", "NoPw", None, Some("/live/slot-03"), true).await;

    let cases = [
        // wrong password
        ("/live/slot-01", "mira-stream", "wrong"),
        // unknown mount
        ("/live/slot-99", "mira-stream", "s3cret"),
        // wrong username on the right mount
        ("/live/slot-01", "someone-else", "s3cret"),
        // login username is not the stream username
        ("/live/slot-01", "mira", "login-pw"),
        // inactive caster, correct credentials
        ("/live/slot-02", "off-stream", "s3cret"),
        // missing stream hash
        ("/live/slot-03", "nopw", "anything"),
    ];

    for (mount, username, password) in cases {
        let (status, body) = send(
            &app(pool.clone()),
            json!({ "mount": mount, "username": username, "password": password }),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "case {mount}/{username}");
        assert_eq!(body["error"]["code"], "unauthorized");
    }
}

/// Issue #105: the route is guarded by the Liquidsoap token, not the portal
/// service token. Missing/wrong Liquidsoap token or a valid service-token
/// bearer must all fail with 401.
#[tokio::test]
async fn requires_liquidsoap_token() {
    let (_dir, pool) = setup().await;
    caster(
        &pool,
        "mira",
        "Mira",
        Some(("mira-stream", "s3cret")),
        Some("/live/slot-01"),
        true,
    )
    .await;
    let body = json!({ "mount": "/live/slot-01", "username": "mira-stream", "password": "s3cret" });
    let app = app(pool);

    // No token at all.
    let (status, _) = send(&app, body.clone(), false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "missing token");

    // Wrong Liquidsoap token.
    let status = send_raw(&app, body.clone(), &[(TOKEN_HEADER, "wrong-token")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong liquidsoap token");

    // The portal service token must NOT grant access here.
    let status = send_raw(
        &app,
        body.clone(),
        &[("authorization", "Bearer test-service-token")],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "service token bearer must be rejected"
    );

    // Sanity: the correct Liquidsoap token still works.
    let (status, _) = send(&app, body, true).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn malformed_body_is_bad_request() {
    let (_dir, pool) = setup().await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/internal/harbor/auth")
        .header(TOKEN_HEADER, liquidsoap_token())
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let response = app(pool).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
