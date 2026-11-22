//! End-to-end tests for the minimal control debug UI (Issue #149) through the
//! real portal router.
//!
//! Explicit proofs:
//!   (a) a session with `control.read` gets `200` on `GET /control/debug`, and
//!       the served HTML advertises the WS bridge (`/api/v1/control/ws`) and the
//!       fader/mute/solo markers,
//!   (b) an unauthenticated request is redirected to `/login` (`303`),
//!   (c) an authenticated session without `control.read` gets `403`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use zukka_portal::api_client::{
    CasterDto, CasterUpdate, CreatedCaster, CreatedSessionDto, IdentityApi, IdentityError,
    NewCaster, SessionValidationDto,
};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

// ---------------------------------------------------------------------------
// Fake identity (same shape as `control_panel_e2e`)
// ---------------------------------------------------------------------------

struct FakeIdentity {
    caster: CasterDto,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new(perms: BTreeMap<String, bool>) -> Self {
        Self {
            caster: CasterDto {
                id: "c-1".into(),
                username: "mira".into(),
                display_name: "Mira".into(),
                email: None,
                role: "caster".into(),
                is_active: true,
                harbor_mount: None,
                stream_username: None,
                permissions: perms,
            },
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
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
        if username == "mira" && password == "pw" {
            Ok(self.caster.clone())
        } else {
            Err(IdentityError::InvalidCredentials)
        }
    }

    async fn create_session(
        &self,
        _caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        let csrf_token = format!("csrf-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: format!("sess-{n}"),
                caster: self.caster.clone(),
                csrf_token: csrf_token.clone(),
                idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
                absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
            },
        );
        Ok(CreatedSessionDto {
            id: format!("sess-{n}"),
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
        Ok(self.sessions.lock().unwrap().get(token).cloned())
    }

    async fn revoke_session(&self, _session_id: &str) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError> {
        if caster_id == self.caster.id {
            Ok(self.caster.clone())
        } else {
            Err(IdentityError::NotFound)
        }
    }

    async fn change_password(
        &self,
        _c: &str,
        _p: &str,
        _k: Option<&str>,
    ) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn change_stream_password(
        &self,
        _c: &str,
        _p: Option<&str>,
    ) -> Result<Option<String>, IdentityError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

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

fn router(perms: BTreeMap<String, bool>) -> Router {
    let state = AppState::with_limiter(
        config(),
        Arc::new(FakeIdentity::new(perms)),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
    );
    build_router(state)
}

fn caster_read_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("caster")
}

/// A role permission map with `control.read` removed (least privilege).
fn without_control_read() -> BTreeMap<String, bool> {
    let mut perms = rbac::permissions_for_role("caster");
    perms.insert("control.read".to_string(), false);
    perms
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

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
    let response = app.clone().oneshot(request).await.expect("response");
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

fn cookie(session: &str) -> String {
    format!("__Host-zukka_session={session}")
}

async fn login(app: &Router) -> String {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let cookie_v = set_cookie_value(&headers, "__Host-zukka_csrf").expect("csrf cookie");
    let token = meta_csrf(&body).expect("csrf meta");
    let payload = form(&[
        ("username", "mira"),
        ("password", "pw"),
        ("csrf_token", &token),
    ]);
    let (status, headers, _) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={cookie_v}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login");
    set_cookie_value(&headers, "__Host-zukka_session").expect("session cookie")
}

async fn get(app: &Router, headers: &[(&str, &str)], uri: &str) -> (StatusCode, String) {
    let (status, _, body) = send(app, Method::GET, uri, headers, None).await;
    (status, body)
}

// ---------------------------------------------------------------------------
// Proof (a): read-capable session renders the debug page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn control_read_session_renders_debug_page() {
    let app = router(caster_read_perms());
    let session = login(&app).await;
    let cookie_header = cookie(&session);

    let (status, body) = get(&app, &[("cookie", &cookie_header)], "/control/debug").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Control Debug"), "{body}");
    assert!(
        body.contains("/api/v1/control/ws"),
        "page must advertise the WS bridge"
    );
    assert!(body.contains("data-role=\"fader\""), "fader marker");
    assert!(body.contains("data-role=\"mute\""), "mute marker");
    assert!(body.contains("data-role=\"solo\""), "solo marker");
    assert!(body.contains("data-role=\"meter-l\""), "meter-L marker");
    assert!(body.contains("data-role=\"meter-r\""), "meter-R marker");
    assert!(body.contains("data-role=\"meter-peak\""), "peak marker");
}

// ---------------------------------------------------------------------------
// Proof (b): unauthenticated GET redirects to /login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_is_redirected_to_login() {
    let app = router(caster_read_perms());
    let (status, _) = get(&app, &[], "/control/debug").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

// ---------------------------------------------------------------------------
// Proof (c): a session without control.read gets 403
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_without_control_read_is_403() {
    let app = router(without_control_read());
    let session = login(&app).await;
    let cookie_header = cookie(&session);

    let (status, _) = get(&app, &[("cookie", &cookie_header)], "/control/debug").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
