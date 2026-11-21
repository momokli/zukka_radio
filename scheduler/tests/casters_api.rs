//! HTTP-level tests for caster management (Issue #101).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router`; the `/internal/casters*` routes are guarded
//! by the service token (§4.1).

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

/// Serialises tests: `ZUKKA_HARBOR_SLOTS` is process-global.
fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn service_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "test-service-token-casters".to_string();
        // SAFETY: guarded by OnceLock; set exactly once.
        unsafe { std::env::set_var(api_auth::SERVICE_TOKEN_ENV, &token) };
        token
    })
}

/// Liquidsoap token (§4.1). Since Issue #105 the `/internal/harbor/auth` route
/// is guarded by this token, not by the portal service token.
fn liquidsoap_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "test-liquidsoap-token-casters".to_string();
        // SAFETY: guarded by OnceLock; set exactly once.
        unsafe { std::env::set_var(api_auth::TOKEN_ENV, &token) };
        token
    })
}

/// POSTs to the Liquidsoap-guarded harbor-auth route (Issue #105).
async fn send_harbor(app: &Router, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/internal/harbor/auth")
        .header("x-liquidsoap-token", liquidsoap_token())
        .header("content-type", "application/json")
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

async fn insert_caster(pool: &SqlitePool, username: &str, mount: Option<&str>) -> String {
    let id = new_id();
    let hash = auth::hash_password("correct horse").expect("hash");
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, harbor_mount, \
         is_active, created_at, updated_at) VALUES (?, ?, 'Caster', ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(&hash)
    .bind(ROLE_CASTER)
    .bind(mount)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    with_service_token: bool,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if with_service_token {
        builder = builder.header("authorization", format!("Bearer {}", service_token()));
    }
    if let Some(actor) = actor {
        builder = builder.header("x-caster-id", actor);
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn create_body(username: &str) -> Value {
    json!({
        "username": username,
        "display_name": format!("Display {username}"),
        "password": "correct horse",
        "role": "caster"
    })
}

#[tokio::test]
async fn caster_routes_require_service_token() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let id = insert_caster(&pool, "mira", None).await;

    for (method, uri, body) in [
        (Method::GET, "/api/v1/internal/casters".to_string(), None),
        (
            Method::POST,
            "/api/v1/internal/casters".to_string(),
            Some(create_body("newbie")),
        ),
        (
            Method::PATCH,
            format!("/api/v1/internal/casters/{id}"),
            Some(json!({ "display_name": "X" })),
        ),
    ] {
        let (status, response) = send(&app, method, &uri, body, false, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "uri: {uri}");
        assert_eq!(response["error"]["code"], "unauthorized");
    }
}

#[tokio::test]
async fn create_caster_returns_one_time_stream_password() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("alice")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["caster"]["username"], "alice");
    assert_eq!(body["caster"]["role"], "caster");
    assert_eq!(body["caster"]["harbor_mount"], "/live/slot-01");
    let stream_password = body["stream_password"].as_str().expect("stream password");
    assert_eq!(stream_password.len(), 24);

    // Login password works.
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "alice", "password": "correct horse" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Harbor credentials work with the generated stream password. The route is
    // Liquidsoap-guarded (Issue #105), so it carries `X-Liquidsoap-Token`.
    let (status, harbor) = send_harbor(
        &app,
        json!({
            "mount": "/live/slot-01",
            "username": "alice",
            "password": stream_password
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {harbor}");
    assert_eq!(harbor["caster_id"], body["caster"]["id"]);

    // Only the Argon2id hash is stored, never the clear text.
    let hash: String =
        sqlx::query_scalar("SELECT stream_password_hash FROM casters WHERE username = ?")
            .bind("alice")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert_ne!(hash, stream_password);
}

#[tokio::test]
async fn duplicate_username_and_mount_are_conflicts() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("alice")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // NOCASE: "ALICE" collides with "alice".
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("ALICE")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"]["code"], "duplicate_username");

    let mut duplicate_mount = create_body("bob");
    duplicate_mount["harbor_mount"] = json!("/live/slot-07");
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(duplicate_mount.clone()),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    duplicate_mount["username"] = json!("carol");
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(duplicate_mount),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"]["code"], "duplicate_harbor_mount");
}

#[tokio::test]
async fn validation_errors_carry_field() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    for (mut body, field) in [
        (create_body("ab"), "username"),
        (
            {
                let mut body = create_body("dave");
                body["role"] = json!("wizard");
                body
            },
            "role",
        ),
        (
            {
                let mut body = create_body("erin");
                body["password"] = json!("short");
                body
            },
            "password",
        ),
        (
            {
                let mut body = create_body("frank");
                body["harbor_mount"] = json!("/live/nope");
                body
            },
            "harbor_mount",
        ),
    ] {
        let (status, response) = send(
            &app,
            Method::POST,
            "/api/v1/internal/casters",
            Some(body.clone()),
            true,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {response}");
        assert_eq!(response["error"]["code"], "validation_error");
        assert_eq!(response["error"]["details"]["field"], field);
        body["username"] = json!("unused");
    }
}

#[tokio::test]
async fn auto_mount_assigns_next_free_slot() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, first) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("first")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {first}");
    assert_eq!(first["caster"]["harbor_mount"], "/live/slot-01");

    let (status, second) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("second")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {second}");
    assert_eq!(second["caster"]["harbor_mount"], "/live/slot-02");
}

#[tokio::test]
async fn exhausted_harbor_pool_is_conflict() {
    let _guard = env_lock().lock().await;
    // SAFETY: serialised by ENV_LOCK.
    unsafe { std::env::set_var(auth::HARBOR_SLOTS_ENV, "2") };

    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    for username in ["one", "two"] {
        let (status, body) = send(
            &app,
            Method::POST,
            "/api/v1/internal/casters",
            Some(create_body(username)),
            true,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
    }

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("three")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"]["code"], "harbor_pool_exhausted");

    unsafe { std::env::remove_var(auth::HARBOR_SLOTS_ENV) };
}

#[tokio::test]
async fn patch_role_revokes_sessions_and_logs_event() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, created) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("gina")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {created}");
    let caster_id = created["caster"]["id"].as_str().unwrap().to_string();

    // A live session before the role change.
    let (status, session) = send(
        &app,
        Method::POST,
        "/api/v1/internal/sessions",
        Some(json!({ "caster_id": caster_id })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {session}");
    let session_id = session["id"].as_str().unwrap().to_string();

    let (status, updated) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/casters/{caster_id}"),
        Some(json!({ "role": "operator" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {updated}");
    assert_eq!(updated["role"], "operator");
    assert_eq!(updated["permissions"]["caster.manage"], true);

    let revoked_at: Option<String> =
        sqlx::query_scalar("SELECT revoked_at FROM sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(revoked_at.is_some(), "role change must revoke sessions");

    let event: String = sqlx::query_scalar(
        "SELECT event FROM event_log WHERE caster_id = ? ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind(&caster_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event, "role_changed");

    // Deactivating blocks login.
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/casters/{caster_id}"),
        Some(json!({ "is_active": false })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/auth/verify",
        Some(json!({ "username": "gina", "password": "correct horse" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_casters_includes_role_permissions() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/casters",
        Some(create_body("helga")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, list) = send(
        &app,
        Method::GET,
        "/api/v1/internal/casters",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {list}");
    let entries = list.as_array().expect("array");
    let helga = entries
        .iter()
        .find(|entry| entry["username"] == "helga")
        .expect("helga listed");
    assert_eq!(helga["permissions"]["queue.write.own"], true);
    assert_eq!(helga["permissions"]["settings.manage"], false);
}

#[tokio::test]
async fn unknown_caster_patch_is_404() {
    let _guard = env_lock().lock().await;
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/casters/{}", new_id()),
        Some(json!({ "display_name": "Nope" })),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["error"]["code"], "not_found");
}
