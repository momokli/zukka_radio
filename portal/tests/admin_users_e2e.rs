//! End-to-end tests for the admin user page (`/admin/users`, Issue #101).
//!
//! The scheduler is replaced by an in-memory [`FakeIdentity`]; router,
//! middleware, RBAC and templates are the real thing.

use std::collections::HashMap;
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

const CREATED_STREAM: &str = "STREAMNEWPASSWORD1234567"; // 24 chars
const GENERATED_STREAM: &str = "GENERATEDPASSWORD1234567"; // 24 chars

struct FakeCaster {
    password: String,
    dto: CasterDto,
}

struct FakeIdentity {
    casters: Mutex<Vec<FakeCaster>>,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new() -> Self {
        let mut casters = Vec::new();
        for (id, username, display, role) in [
            ("a-1", "admin1", "Admin One", "admin"),
            ("o-1", "operator1", "Operator One", "operator"),
            ("c-1", "caster1", "Caster One", "caster"),
        ] {
            casters.push(FakeCaster {
                password: format!("pw-{username}"),
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
            counter: Mutex::new(0),
        }
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        let casters = self.casters.lock().unwrap();
        match casters
            .iter()
            .find(|caster| caster.dto.username.eq_ignore_ascii_case(username))
        {
            Some(caster) if caster.dto.is_active && caster.password == password => {
                Ok(caster.dto.clone())
            }
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
            .find(|caster| caster.dto.id == caster_id)
            .map(|caster| caster.dto.clone())
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
        Ok(self.sessions.lock().unwrap().get(token).cloned())
    }

    async fn revoke_session(&self, session_id: &str) -> Result<(), IdentityError> {
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, validation| validation.session_id != session_id);
        Ok(())
    }

    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|caster| caster.dto.id == caster_id)
            .map(|caster| caster.dto.clone())
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
            .find(|caster| caster.dto.id == caster_id)
            .ok_or(IdentityError::NotFound)?;
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
            .find(|caster| caster.dto.id == caster_id)
            .ok_or(IdentityError::NotFound)?;
        match password {
            Some(password) => {
                let _ = (caster, password);
                Ok(None)
            }
            None => Ok(Some(GENERATED_STREAM.to_string())),
        }
    }

    async fn list_casters(&self) -> Result<Vec<CasterDto>, IdentityError> {
        Ok(self
            .casters
            .lock()
            .unwrap()
            .iter()
            .map(|caster| caster.dto.clone())
            .collect())
    }

    async fn create_caster(
        &self,
        _actor: Option<&str>,
        req: &NewCaster,
    ) -> Result<CreatedCaster, IdentityError> {
        let mut casters = self.casters.lock().unwrap();
        if casters
            .iter()
            .any(|caster| caster.dto.username.eq_ignore_ascii_case(&req.username))
        {
            return Err(IdentityError::Conflict("duplicate_username".into()));
        }
        if let Some(mount) = &req.harbor_mount {
            if casters
                .iter()
                .any(|caster| caster.dto.harbor_mount.as_deref() == Some(mount.as_str()))
            {
                return Err(IdentityError::Conflict("duplicate_harbor_mount".into()));
            }
        }
        let mount = req.harbor_mount.clone().or_else(|| {
            (1..=32)
                .map(|slot| format!("/live/slot-{slot:02}"))
                .find(|candidate| {
                    !casters.iter().any(|caster| {
                        caster.dto.harbor_mount.as_deref() == Some(candidate.as_str())
                    })
                })
        });
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let dto = CasterDto {
            id: format!("new-{counter}"),
            username: req.username.clone(),
            display_name: req.display_name.clone(),
            email: req.email.clone(),
            role: req.role.clone(),
            is_active: true,
            harbor_mount: mount,
            stream_username: Some(req.username.clone()),
            permissions: rbac::permissions_for_role(&req.role),
        };
        casters.push(FakeCaster {
            password: req.password.clone(),
            dto: dto.clone(),
        });
        Ok(CreatedCaster {
            caster: dto,
            stream_password: Some(CREATED_STREAM.to_string()),
        })
    }

    async fn update_caster(
        &self,
        _actor: Option<&str>,
        id: &str,
        patch: &CasterUpdate,
    ) -> Result<CasterDto, IdentityError> {
        let mut casters = self.casters.lock().unwrap();
        let caster = casters
            .iter_mut()
            .find(|caster| caster.dto.id == id)
            .ok_or(IdentityError::NotFound)?;
        let mut role_changed = false;
        if let Some(role) = &patch.role {
            if caster.dto.role != *role {
                caster.dto.role = role.clone();
                caster.dto.permissions = rbac::permissions_for_role(role);
                role_changed = true;
            }
        }
        if let Some(active) = patch.is_active {
            caster.dto.is_active = active;
        }
        if let Some(display_name) = &patch.display_name {
            caster.dto.display_name = display_name.clone();
        }
        if let Some(email) = &patch.email {
            caster.dto.email = (!email.is_empty()).then(|| email.clone());
        }
        let dto = caster.dto.clone();
        drop(casters);
        if role_changed {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.retain(|_, validation| validation.caster.id != id);
        }
        Ok(dto)
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
        let raw = value.to_str().ok()?;
        if let Some(rest) = raw.strip_prefix(&format!("{name}=")) {
            return Some(rest.split(';').next().unwrap_or_default().to_string());
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
        .map(|(key, value)| format!("{key}={value}"))
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

fn session_header(session: &str) -> String {
    format!("__Host-zukka_session={session}")
}

#[tokio::test]
async fn admin_page_requires_caster_manage() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));

    let (caster_session, _) = login_session(&app, "caster1", "pw-caster1").await;
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &session_header(&caster_session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (admin_session, _) = login_session(&app, "admin1", "pw-admin1").await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &session_header(&admin_session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Neuen Caster anlegen"), "body: {body}");
    assert!(body.contains("caster1"), "body lists casters");
}

#[tokio::test]
async fn create_requires_csrf() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));
    let (session, _) = login_session(&app, "admin1", "pw-admin1").await;

    let body = form(&[
        ("username", "newbie"),
        ("display_name", "Newbie"),
        ("role", "caster"),
        ("password", "initial-password"),
    ]);
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/admin/users",
        &[
            ("cookie", &session_header(&session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn create_shows_stream_password_exactly_once() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));
    let (session, csrf) = login_session(&app, "admin1", "pw-admin1").await;

    let create_body = |username: &str| {
        form(&[
            ("username", username),
            ("display_name", "New Person"),
            ("role", "caster"),
            ("password", "initial-password"),
            ("csrf_token", &csrf),
        ])
    };
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/admin/users",
        &[
            ("cookie", &session_header(&session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(create_body("newbie")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body.contains(CREATED_STREAM), "one-time password shown");

    // A reload of the list no longer contains the password.
    let (status, _, page) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &session_header(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.contains(CREATED_STREAM), "password must not persist");

    // Duplicate username → inline 409, no panic.
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/admin/users",
        &[
            ("cookie", &session_header(&session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(create_body("newbie")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(body.contains("bereits vergeben"), "body: {body}");
}

#[tokio::test]
async fn update_role_and_guard_self_and_last_admin() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));
    let (admin_session, admin_csrf) = login_session(&app, "admin1", "pw-admin1").await;

    // Promote caster1 to operator.
    let body = form(&[
        ("display_name", "Caster One"),
        ("role", "operator"),
        ("is_active", "true"),
        ("csrf_token", &admin_csrf),
    ]);
    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/admin/users/c-1",
        &[
            ("cookie", &session_header(&admin_session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &admin_csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/admin/users");

    let (_, _, page) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &session_header(&admin_session))],
        None,
    )
    .await;
    assert!(page.contains("operator"), "role updated");

    // Self-demotion is refused.
    let body = form(&[
        ("display_name", "Admin One"),
        ("role", "caster"),
        ("is_active", "true"),
        ("csrf_token", &admin_csrf),
    ]);
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/admin/users/a-1",
        &[
            ("cookie", &session_header(&admin_session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &admin_csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(body.contains("degradiert"), "body: {body}");

    // An operator cannot demote the last active admin.
    let (operator_session, operator_csrf) = login_session(&app, "operator1", "pw-operator1").await;
    let body = form(&[
        ("display_name", "Admin One"),
        ("role", "caster"),
        ("is_active", "true"),
        ("csrf_token", &operator_csrf),
    ]);
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/admin/users/a-1",
        &[
            ("cookie", &session_header(&operator_session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &operator_csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(body.contains("letzte aktive Admin"), "body: {body}");
}

#[tokio::test]
async fn stream_password_generated_once() {
    let fake = Arc::new(FakeIdentity::new());
    let app = build_router(state(fake));
    let (session, csrf) = login_session(&app, "admin1", "pw-admin1").await;

    let body = form(&[("password", ""), ("csrf_token", &csrf)]);
    let (status, _, page) = send(
        &app,
        Method::POST,
        "/admin/users/c-1/stream-password",
        &[
            ("cookie", &session_header(&session)),
            ("content-type", "application/x-www-form-urlencoded"),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "page: {page}");
    assert!(page.contains(GENERATED_STREAM), "generated password shown");

    let (_, _, page) = send(
        &app,
        Method::GET,
        "/admin/users",
        &[("cookie", &session_header(&session))],
        None,
    )
    .await;
    assert!(!page.contains(GENERATED_STREAM), "password not persisted");
}
