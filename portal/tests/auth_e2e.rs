//! Cross-crate end-to-end tests for Issue #16 "Auth".
//!
//! Unlike `portal_flow.rs` (which stubs the scheduler with an in-memory
//! `FakeIdentity`), these tests wire the **real** portal router to the **real**
//! scheduler identity authority over a TCP socket: a temporary SQLite database
//! is migrated, a caster is seeded with an Argon2id hash, `zukka_scheduler::api::router`
//! is served on an ephemeral port, and `HttpIdentityApi` talks to it. Every
//! assertion therefore exercises the true portal↔scheduler contract: Argon2id
//! verify, server-side sessions, CSRF derivation, rotation and the D2
//! login/stream-password inequality rule.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Extension;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_portal::api_client::HttpIdentityApi;
use zukka_portal::app::{AppState, build_router};
use zukka_portal::auth::AuthContext;
use zukka_portal::config::Config;
use zukka_portal::error::PortalError;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

use zukka_scheduler::api::auth as sched_auth;
use zukka_scheduler::store::auth as sched_store;
use zukka_scheduler::uuid::new_id;

/// Shared portal↔scheduler service token for the whole test binary.
const SERVICE_TOKEN: &str = "e2e-service-token";

/// Seeded role ids (§6.10.1, migration `0001_init.sql`).
const ROLE_ADMIN: &str = "0192f0a1-0001-7000-8000-000000000001";
const ROLE_OPERATOR: &str = "0192f0a1-0001-7000-8000-000000000002";
const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";

const TS: &str = "2026-11-18T12:00:00.000Z";

/// Installs the service token once (the scheduler reads it at request time).
fn set_service_token() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: set exactly once for the whole test binary, before any request.
        unsafe { std::env::set_var(sched_auth::SERVICE_TOKEN_ENV, SERVICE_TOKEN) };
    });
}

fn config(scheduler_base_url: String) -> Config {
    Config::new(
        "127.0.0.1:8081".into(),
        "https://portal.example.org".into(),
        scheduler_base_url,
        SERVICE_TOKEN.into(),
        7,
        30,
    )
    .expect("valid config")
}

/// Seeds an active caster with an Argon2id password hash.
async fn seed_caster(pool: &SqlitePool, username: &str, password: &str, role_id: &str) -> String {
    let id = new_id();
    let hash = sched_store::hash_password(password).expect("hash password");
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(format!("Display {username}"))
    .bind(&hash)
    .bind(role_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

/// A live scheduler + portal pair sharing one temporary database.
struct Harness {
    _dir: TempDir,
    state: AppState,
    pool: SqlitePool,
}

impl Harness {
    /// Boots the scheduler on an ephemeral port with the given seeded casters.
    async fn boot(casters: &[(&str, &str, &str)]) -> Self {
        set_service_token();
        sched_auth::reset_login_limiter();

        let dir = tempfile::tempdir().expect("tempdir");
        let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
            .await
            .expect("connect + migrate");
        for (username, password, role) in casters {
            seed_caster(&pool, username, password, role).await;
        }

        let sched_app = zukka_scheduler::api::router(pool.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scheduler");
        let addr = listener.local_addr().expect("scheduler addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, sched_app).await;
        });

        let identity =
            HttpIdentityApi::new(format!("http://{addr}"), SERVICE_TOKEN).expect("identity client");
        let state = AppState::with_limiter(
            config(format!("http://{addr}")),
            Arc::new(identity),
            Arc::new(WindowLimiter::new(5, Duration::from_secs(900))),
        );

        Self {
            _dir: dir,
            state,
            pool,
        }
    }

    /// The production portal router.
    fn router(&self) -> Router {
        build_router(self.state.clone())
    }

    /// Direct read of the scheduler's session row count, to prove revocation.
    async fn active_sessions(&self, caster_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM sessions WHERE caster_id = ? AND revoked_at IS NULL",
        )
        .bind(caster_id)
        .fetch_one(&self.pool)
        .await
        .expect("count sessions")
    }
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<String>,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(body) => builder.body(Body::from(body)).expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    };
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

fn set_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        let first = raw.split(';').next().unwrap_or("");
        if let Some(rest) = first.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn full_set_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        if raw.starts_with(&format!("{name}=")) {
            return Some(raw.to_string());
        }
    }
    None
}

fn meta_csrf(body: &str) -> Option<String> {
    let marker = "name=\"csrf-token\" content=\"";
    let start = body.find(marker)? + marker.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Logs in through the real portal and returns `(session_cookie, csrf_token)`.
async fn login_session(app: &Router, username: &str, password: &str) -> (String, String) {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let csrf_cookie = set_cookie_value(&headers, "__Host-zukka_csrf").expect("csrf cookie");
    let token = meta_csrf(&body).expect("csrf meta");
    let payload = form(&[
        ("username", username),
        ("password", password),
        ("csrf_token", &token),
    ]);
    let (status, headers, body) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={csrf_cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login {username}: {body}");
    let session = set_cookie_value(&headers, "__Host-zukka_session").expect("session cookie");
    let (status, _, dashboard) = send(
        app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dashboard after login");
    let csrf = meta_csrf(&dashboard).expect("dashboard csrf");
    (session, csrf)
}

/// Attempts a login with a chosen client IP; returns the status code.
async fn login_with_ip(app: &Router, username: &str, password: &str, ip: &str) -> StatusCode {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let csrf_cookie = set_cookie_value(&headers, "__Host-zukka_csrf").expect("csrf cookie");
    let token = meta_csrf(&body).expect("csrf meta");
    let payload = form(&[
        ("username", username),
        ("password", password),
        ("csrf_token", &token),
    ]);
    let (status, _, _) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={csrf_cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-forwarded-for", ip),
        ],
        Some(payload),
    )
    .await;
    status
}

// ---------------------------------------------------------------------------
// 1. Login flow + cookie attributes + dashboard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_sets_host_cookie_and_reaches_dashboard() {
    let h = Harness::boot(&[("mira", "correct-horse", ROLE_CASTER)]).await;
    let app = h.router();

    // GET /login issues the double-submit cookie and embeds the same value.
    let (status, headers, body) = send(&app, Method::GET, "/login", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let csrf_cookie = set_cookie_value(&headers, "__Host-zukka_csrf").unwrap();
    assert_eq!(meta_csrf(&body).as_deref(), Some(csrf_cookie.as_str()));

    // POST /login with a real Argon2id verify against the scheduler → 303.
    let payload = form(&[
        ("username", "mira"),
        ("password", "correct-horse"),
        ("csrf_token", &csrf_cookie),
    ]);
    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={csrf_cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/");

    let set_cookie = full_set_cookie(&headers, "__Host-zukka_session").unwrap();
    assert!(
        set_cookie.starts_with("__Host-zukka_session="),
        "{set_cookie}"
    );
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/"), "{set_cookie}");
    assert!(!set_cookie.contains("Domain"), "{set_cookie}");
    let session = set_cookie_value(&headers, "__Host-zukka_session").unwrap();

    // GET / with the cookie → 200 and the seeded display name.
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Display mira"), "dashboard body: {body}");
    assert!(body.contains("caster"), "role on dashboard: {body}");
}

// ---------------------------------------------------------------------------
// 2. CSRF: non-GET without / with a wrong token → 403, even with a session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn csrf_blocks_writes_even_with_valid_session() {
    let h = Harness::boot(&[("mira", "pw-mira", ROLE_CASTER)]).await;
    let app = h.router();
    let (session, csrf) = login_session(&app, "mira", "pw-mira").await;

    // No token → 403 (not a redirect), despite a valid session.
    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        headers.get(header::LOCATION).is_none(),
        "no redirect on 403"
    );

    // Wrong token → 403.
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", "wrong-token"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Cross-origin signal + valid token → 403 as well.
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "cross-site"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Correct token + same-origin → 303.
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

// ---------------------------------------------------------------------------
// 3. RBAC: caster → 403 on /control + /admin/*; operator/admin allowed
// ---------------------------------------------------------------------------

async fn control_write(Extension(ctx): Extension<AuthContext>) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.write")?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn admin_users(Extension(ctx): Extension<AuthContext>) -> Result<Response, PortalError> {
    rbac::check(&ctx, "caster.manage")?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn guarded_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(zukka_portal::auth::handlers::dashboard))
        .route("/control", post(control_write))
        .route("/admin/users", get(admin_users))
        .route_layer(from_fn_with_state(
            state.clone(),
            zukka_portal::auth::middleware::require_csrf,
        ))
        .route_layer(from_fn_with_state(
            state.clone(),
            zukka_portal::auth::middleware::resolve_session,
        ))
        .route(
            "/login",
            get(zukka_portal::auth::handlers::login_get)
                .post(zukka_portal::auth::handlers::login_post),
        )
        .with_state(state)
}

#[tokio::test]
async fn rbac_permission_matrix_over_real_sessions() {
    let h = Harness::boot(&[
        ("caster-x", "pw", ROLE_CASTER),
        ("operator-x", "pw", ROLE_OPERATOR),
        ("admin-x", "pw", ROLE_ADMIN),
    ])
    .await;
    let app = guarded_router(h.state.clone());

    // caster → 403 on both control.write and caster.manage (valid CSRF).
    let (cs, cc) = login_session(&app, "caster-x", "pw").await;
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/control",
        &[
            ("cookie", &format!("__Host-zukka_session={cs}")),
            ("x-csrf-token", &cc),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "caster POST /control");
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &format!("__Host-zukka_session={cs}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "caster GET /admin/users");

    // operator → 204 on both.
    let (os, oc) = login_session(&app, "operator-x", "pw").await;
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/control",
        &[
            ("cookie", &format!("__Host-zukka_session={os}")),
            ("x-csrf-token", &oc),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "operator POST /control");
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &format!("__Host-zukka_session={os}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "operator GET /admin/users");

    // admin → 204 on control.write.
    let (as_, ac) = login_session(&app, "admin-x", "pw").await;
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/control",
        &[
            ("cookie", &format!("__Host-zukka_session={as_}")),
            ("x-csrf-token", &ac),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "admin POST /control");
}

// ---------------------------------------------------------------------------
// 4. Logout revokes the session and clears the cookie
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logout_revokes_session_and_clears_cookie() {
    let h = Harness::boot(&[("mira", "pw-mira", ROLE_CASTER)]).await;
    let app = h.router();
    let (session, csrf) = login_session(&app, "mira", "pw-mira").await;
    assert_eq!(h.active_sessions(&caster_id(&h, "mira").await).await, 1);

    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/login");

    // The clearing cookie is issued with Max-Age=0.
    let clear = full_set_cookie(&headers, "__Host-zukka_session").unwrap();
    assert!(clear.contains("Max-Age=0"), "clearing cookie: {clear}");

    // The scheduler actually revoked the row.
    assert_eq!(h.active_sessions(&caster_id(&h, "mira").await).await, 0);

    // The old cookie is no longer accepted → redirect to /login.
    let (status, headers, _) = send(
        &app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/login");
}

async fn caster_id(h: &Harness, username: &str) -> String {
    sqlx::query_scalar("SELECT id FROM casters WHERE username = ?")
        .bind(username)
        .fetch_one(&h.pool)
        .await
        .expect("caster id")
}

// ---------------------------------------------------------------------------
// 5. Rotation: new token per login; password change revokes other sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_rotates_token_and_password_change_revokes_other_sessions() {
    let h = Harness::boot(&[("mira", "old-pw", ROLE_CASTER)]).await;
    let app = h.router();

    // Two independent browser sessions.
    let (s1, c1) = login_session(&app, "mira", "old-pw").await;
    let (s2, _c2) = login_session(&app, "mira", "old-pw").await;
    assert_ne!(s1, s2, "each login issues a fresh session token");
    assert_eq!(h.active_sessions(&caster_id(&h, "mira").await).await, 2);

    // Change the password from session 1; session 1 survives, session 2 dies.
    let payload = form(&[
        ("current_password", "old-pw"),
        ("new_password", "new-pw"),
        ("csrf_token", &c1),
    ]);
    let (status, headers, body) = send(
        &app,
        Method::POST,
        "/account/password",
        &[
            ("cookie", &format!("__Host-zukka_session={s1}")),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &c1),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "password change: {body}");
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/account");
    assert_eq!(
        h.active_sessions(&caster_id(&h, "mira").await).await,
        1,
        "only the kept session stays active"
    );

    let (status, _, _) = send(
        &app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={s1}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "kept session remains valid");

    let (status, headers, _) = send(
        &app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={s2}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "other session revoked");
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/login");

    // Old password no longer works; the new one does.
    assert_eq!(
        login_with_ip(&app, "mira", "old-pw", "198.51.100.5").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login_with_ip(&app, "mira", "new-pw", "198.51.100.5").await,
        StatusCode::SEE_OTHER
    );
}

// ---------------------------------------------------------------------------
// 6. Rate limit: 5 failures / 15 min per IP+username → 429
// ---------------------------------------------------------------------------

#[tokio::test]
async fn five_failed_logins_per_ip_and_username_yield_429() {
    let h = Harness::boot(&[("mira", "correct", ROLE_CASTER)]).await;
    let app = h.router();
    let ip = "203.0.113.7";

    for _ in 0..5 {
        assert_eq!(
            login_with_ip(&app, "mira", "wrong", ip).await,
            StatusCode::UNAUTHORIZED
        );
    }
    // 6th attempt → 429, even with the correct password and the same IP.
    assert_eq!(
        login_with_ip(&app, "mira", "correct", ip).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // A different IP is unaffected.
    assert_eq!(
        login_with_ip(&app, "mira", "correct", "203.0.113.8").await,
        StatusCode::SEE_OTHER
    );
}

// ---------------------------------------------------------------------------
// 7. Stream password == login password → 422 in both directions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_password_must_differ_from_login_password_both_directions() {
    let h = Harness::boot(&[("mira", "login-pw", ROLE_CASTER)]).await;
    let app = h.router();
    let (session, csrf) = login_session(&app, "mira", "login-pw").await;
    let cookie = format!("__Host-zukka_session={session}");

    // Set an explicit stream password (differs from the login password).
    let payload = form(&[("password", "stream-secret"), ("csrf_token", &csrf)]);
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/account/stream-password",
        &[
            ("cookie", &cookie),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &csrf),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Direction A: new login password equal to the stream password → 422.
    let payload = form(&[
        ("current_password", "login-pw"),
        ("new_password", "stream-secret"),
        ("csrf_token", &csrf),
    ]);
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/account/password",
        &[
            ("cookie", &cookie),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &csrf),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(body.contains("must differ"), "body: {body}");

    // Direction B: stream password equal to the (unchanged) login password → 422.
    let payload = form(&[("password", "login-pw"), ("csrf_token", &csrf)]);
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/account/stream-password",
        &[
            ("cookie", &cookie),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &csrf),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(body.contains("must differ"), "body: {body}");
}
