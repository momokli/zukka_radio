//! End-to-end portal tests: login → dashboard → logout, CSRF, RBAC, rate limit.
//!
//! The scheduler is replaced by an in-memory [`FakeIdentity`]; everything else
//! (router, middleware, templates) is the real thing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Extension;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use tower::ServiceExt;

use zukka_portal::api_client::{
    CasterDto, CreatedSessionDto, IdentityApi, IdentityError, SessionValidationDto,
};
use zukka_portal::api_client::{CasterUpdate, CreatedCaster, NewCaster};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::auth::AuthContext;
use zukka_portal::config::Config;
use zukka_portal::error::PortalError;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

const GENERATED: &str = "AAAABBBBCCCCDDDDEEEEFFFF"; // 24 chars

struct FakeCaster {
    password: String,
    stream_password: Option<String>,
    dto: CasterDto,
}

/// In-memory scheduler stand-in.
struct FakeIdentity {
    casters: Mutex<Vec<FakeCaster>>,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    revoked: Mutex<HashSet<String>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new() -> Self {
        let mut casters = Vec::new();
        for (id, username, display, role) in [
            ("c-1", "caster1", "Caster One", "caster"),
            ("o-1", "operator1", "Operator One", "operator"),
            ("a-1", "admin1", "Admin One", "admin"),
        ] {
            casters.push(FakeCaster {
                password: format!("pw-{username}"),
                stream_password: None,
                dto: CasterDto {
                    id: id.to_string(),
                    username: username.to_string(),
                    display_name: display.to_string(),
                    email: None,
                    role: role.to_string(),
                    is_active: true,
                    harbor_mount: Some(format!("/live/{username}")),
                    stream_username: Some(username.to_string()),
                    permissions: rbac::permissions_for_role(role),
                },
            });
        }
        Self {
            casters: Mutex::new(casters),
            sessions: Mutex::new(HashMap::new()),
            revoked: Mutex::new(HashSet::new()),
            counter: Mutex::new(0),
        }
    }

    fn with_stream_password(self, username: &str, password: &str) -> Self {
        let mut casters = self.casters.lock().unwrap();
        if let Some(c) = casters.iter_mut().find(|c| c.dto.username == username) {
            c.stream_password = Some(password.to_string());
        }
        drop(casters);
        self
    }

    fn is_revoked(&self, session_id: &str) -> bool {
        self.revoked.lock().unwrap().contains(session_id)
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn list_casters(&self) -> Result<Vec<CasterDto>, IdentityError> {
        Ok(Vec::new())
    }

    async fn create_caster(
        &self,
        _actor: Option<&str>,
        _req: &NewCaster,
    ) -> Result<CreatedCaster, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }

    async fn update_caster(
        &self,
        _actor: Option<&str>,
        _id: &str,
        _patch: &CasterUpdate,
    ) -> Result<CasterDto, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        let casters = self.casters.lock().unwrap();
        match casters
            .iter()
            .find(|c| c.dto.username.eq_ignore_ascii_case(username))
        {
            Some(c) if c.dto.is_active && c.password == password => Ok(c.dto.clone()),
            _ => Err(IdentityError::InvalidCredentials),
        }
    }

    async fn create_session(
        &self,
        caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        let caster = self
            .casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.id == caster_id)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::NotFound)?;
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        let csrf_token = format!("csrf-{n}");
        let session_id = format!("sess-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: session_id.clone(),
                caster,
                csrf_token: csrf_token.clone(),
                idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
                absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
            },
        );
        Ok(CreatedSessionDto {
            id: session_id,
            token,
            csrf_token,
            idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
            absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
        })
    }

    async fn validate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError> {
        let sessions = self.sessions.lock().unwrap();
        match sessions.get(token) {
            Some(validation) if !self.is_revoked(&validation.session_id) => {
                Ok(Some(validation.clone()))
            }
            _ => Ok(None),
        }
    }

    async fn revoke_session(&self, session_id: &str) -> Result<(), IdentityError> {
        self.revoked.lock().unwrap().insert(session_id.to_string());
        Ok(())
    }

    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.id == caster_id)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::NotFound)
    }

    async fn change_password(
        &self,
        caster_id: &str,
        password: &str,
        _keep: Option<&str>,
    ) -> Result<(), IdentityError> {
        let mut casters = self.casters.lock().unwrap();
        let caster = casters
            .iter_mut()
            .find(|c| c.dto.id == caster_id)
            .ok_or(IdentityError::NotFound)?;
        if caster.stream_password.as_deref() == Some(password) {
            return Err(IdentityError::Validation {
                field: "password".into(),
                message: "login password must differ from the stream password".into(),
            });
        }
        caster.password = password.to_string();
        Ok(())
    }

    async fn change_stream_password(
        &self,
        caster_id: &str,
        password: Option<&str>,
    ) -> Result<Option<String>, IdentityError> {
        let mut casters = self.casters.lock().unwrap();
        let caster = casters
            .iter_mut()
            .find(|c| c.dto.id == caster_id)
            .ok_or(IdentityError::NotFound)?;
        match password {
            Some(password) => {
                if caster.password == password {
                    return Err(IdentityError::Validation {
                        field: "stream_password".into(),
                        message: "stream password must differ from the login password".into(),
                    });
                }
                caster.stream_password = Some(password.to_string());
                Ok(None)
            }
            None => {
                caster.stream_password = Some(GENERATED.to_string());
                Ok(Some(GENERATED.to_string()))
            }
        }
    }
}

fn config() -> Config {
    Config::new(
        "127.0.0.1:8081".into(),
        "https://portal.example.org".into(),
        "http://scheduler:8080".into(),
        "svc-token".into(),
        7,
        30,
    )
    .unwrap()
}

fn state(fake: Arc<FakeIdentity>) -> AppState {
    AppState::with_limiter(
        config(),
        fake,
        Arc::new(WindowLimiter::new(5, Duration::from_secs(900))),
    )
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
        Some(body) => builder.body(Body::from(body)).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
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

/// Logs in and returns `(session_cookie_value, csrf_token)`.
async fn login_session(app: &Router, username: &str, password: &str) -> (String, String) {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let login_cookie = set_cookie_value(&headers, "__Host-zukka_csrf").unwrap();
    let token = meta_csrf(&body).unwrap();
    let body = form(&[
        ("username", username),
        ("password", password),
        ("csrf_token", &token),
    ]);
    let (status, headers, _) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={login_cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login {username}");
    let session = set_cookie_value(&headers, "__Host-zukka_session").unwrap();
    let (_, _, dashboard) = send(
        app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    let csrf = meta_csrf(&dashboard).unwrap();
    (session, csrf)
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_dashboard_logout_flow() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake.clone()));

    // GET /login issues the double-submit cookie and embeds the same value.
    let (status, headers, body) = send(&app, Method::GET, "/login", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let login_cookie = set_cookie_value(&headers, "__Host-zukka_csrf").unwrap();
    assert_eq!(meta_csrf(&body).as_deref(), Some(login_cookie.as_str()));

    // POST /login → 303 + session cookie.
    let body = form(&[
        ("username", "caster1"),
        ("password", "pw-caster1"),
        ("csrf_token", &login_cookie),
    ]);
    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={login_cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/");

    let set_cookie = full_set_cookie(&headers, "__Host-zukka_session").unwrap();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/"), "{set_cookie}");
    assert!(!set_cookie.contains("Domain"), "{set_cookie}");
    let session = set_cookie_value(&headers, "__Host-zukka_session").unwrap();

    // Dashboard is reachable and shows the caster.
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Caster One"), "dashboard body");
    let csrf = meta_csrf(&body).unwrap();

    // Logout revokes the session and clears the cookie.
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
    assert!(fake.is_revoked("sess-1"));

    // The old cookie no longer works.
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

#[tokio::test]
async fn wrong_password_and_unknown_user_are_identical_401() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));

    async fn attempt(app: &Router, username: &str, password: &str) -> (StatusCode, String) {
        let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
        let cookie = set_cookie_value(&headers, "__Host-zukka_csrf").unwrap();
        let token = meta_csrf(&body).unwrap();
        let body = form(&[
            ("username", username),
            ("password", password),
            ("csrf_token", &token),
        ]);
        let (status, _, body) = send(
            app,
            Method::POST,
            "/login",
            &[
                ("cookie", &format!("__Host-zukka_csrf={cookie}")),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            Some(body),
        )
        .await;
        (status, body)
    }

    let (wrong_status, wrong_body) = attempt(&app, "caster1", "nope").await;
    let (unknown_status, unknown_body) = attempt(&app, "ghost", "nope").await;
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_status, StatusCode::UNAUTHORIZED);
    assert!(wrong_body.contains("Benutzername oder Passwort ist falsch."));
    assert!(unknown_body.contains("Benutzername oder Passwort ist falsch."));
}

#[tokio::test]
async fn login_rate_limit_returns_429() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));

    let (_, headers, body) = send(&app, Method::GET, "/login", &[], None).await;
    let cookie = set_cookie_value(&headers, "__Host-zukka_csrf").unwrap();
    let token = meta_csrf(&body).unwrap();
    let body = form(&[
        ("username", "caster1"),
        ("password", "bad"),
        ("csrf_token", &token),
    ]);

    for _ in 0..5 {
        let (status, _, _) = send(
            &app,
            Method::POST,
            "/login",
            &[
                ("cookie", &format!("__Host-zukka_csrf={cookie}")),
                ("content-type", "application/x-www-form-urlencoded"),
                ("x-forwarded-for", "203.0.113.7"),
            ],
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // 6th attempt → 429, even with the correct password.
    let good = form(&[
        ("username", "caster1"),
        ("password", "pw-caster1"),
        ("csrf_token", &token),
    ]);
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-forwarded-for", "203.0.113.7"),
        ],
        Some(good),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn missing_csrf_token_is_403_not_redirect() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));
    let (session, csrf) = login_session(&app, "caster1", "pw-caster1").await;

    // Valid session, but no CSRF token → 403 (not a redirect).
    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(headers.get(header::LOCATION).is_none());

    // Wrong CSRF token → 403.
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

    // Correct CSRF token → 303.
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/logout",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", &csrf),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn unauthenticated_requests() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));

    let (status, headers, _) = send(&app, Method::GET, "/", &[], None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/login");

    let (status, _, _) = send(&app, Method::POST, "/logout", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// RBAC guard (permission matrix §6.10.1) wired to the canonical routes.
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
async fn caster_is_forbidden_control_and_admin() {
    let fake = Arc::new(FakeIdentity::new());
    let app = guarded_router(state(fake));

    let (session, csrf) = login_session(&app, "caster1", "pw-caster1").await;

    // POST /control → 403 for a caster (valid CSRF, so RBAC is what rejects).
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/control",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", &csrf),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // GET /admin/users → 403 for a caster.
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An operator may write control but not manage settings/admin.
    let (op_session, op_csrf) = login_session(&app, "operator1", "pw-operator1").await;
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/control",
        &[
            ("cookie", &format!("__Host-zukka_session={op_session}")),
            ("x-csrf-token", &op_csrf),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &format!("__Host-zukka_session={op_session}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// Account: password / stream password
// ---------------------------------------------------------------------------

#[tokio::test]
async fn password_change_equal_to_stream_password_is_422() {
    let fake = Arc::new(FakeIdentity::new().with_stream_password("caster1", "stream-secret"));
    let app = build_router(state(fake));
    let (session, csrf) = login_session(&app, "caster1", "pw-caster1").await;

    let body = form(&[
        ("current_password", "pw-caster1"),
        ("new_password", "stream-secret"),
        ("csrf_token", &csrf),
    ]);
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/account/password",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(body.contains("must differ"), "body: {body}");
}

#[tokio::test]
async fn generated_stream_password_is_shown_once() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake.clone()));
    let (session, csrf) = login_session(&app, "caster1", "pw-caster1").await;

    let body = form(&[("password", ""), ("csrf_token", &csrf)]);
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/account/stream-password",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains(GENERATED), "generated password shown once");

    // The next GET no longer shows it.
    let (_, _, body) = send(
        &app,
        Method::GET,
        "/account",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    assert!(
        !body.contains(GENERATED),
        "password must not persist in the UI"
    );
}
