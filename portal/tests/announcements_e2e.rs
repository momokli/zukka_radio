//! End-to-end tests for the manual announcements page (Issue #38) through the
//! real portal router.
//!
//! The scheduler is replaced by [`FakeAnnouncementApi`] (which enforces the
//! same `event="manual"` scope as the real client); login uses the real session
//! cookie + CSRF flow (mirrors `schedule_ui_e2e.rs`). Explicit proofs:
//!   (a) `GET /announcements` renders the post form when the session grants
//!       `announcement.write`, and is a `403` without it,
//!   (b) `POST /announcements/post` without `announcement.write` is a `403`
//!       and never reaches the client (no create),
//!   (c) a permitted post performs exactly one create, and the `caster_id`
//!       recorded by the client is the session identity (never form input, §1),
//!   (d) non-htmx posts redirect, htmx posts answer `204` + `HX-Refresh`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use zukka_portal::api_client::{
    AnnouncementApi, CasterDto, CreatedSessionDto, FakeAnnouncementApi, FakeControlApi,
    FakeQueueApi, FakeScheduleBackend, FakeTrackApi, IdentityApi, IdentityError, JobError,
    NewUploadJob, SessionValidationDto, UploadJob, UploadJobApi, UploadJobPatch, Waveform,
    WaveformApi, WaveformApiError,
};
use zukka_portal::api_client::{CasterUpdate, CreatedCaster, NewCaster};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

// ---------------------------------------------------------------------------
// Fake identity (one caster whose permission map is configurable)
// ---------------------------------------------------------------------------

struct FakeIdentity {
    dto: CasterDto,
    sessions: Mutex<std::collections::HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new(perms: BTreeMap<String, bool>) -> Self {
        Self {
            dto: CasterDto {
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
            sessions: Mutex::new(std::collections::HashMap::new()),
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
        if self.dto.username == username && password == "pw" {
            Ok(self.dto.clone())
        } else {
            Err(IdentityError::InvalidCredentials)
        }
    }

    async fn create_session(
        &self,
        caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        if self.dto.id != caster_id {
            return Err(IdentityError::NotFound);
        }
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        let csrf_token = format!("csrf-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: format!("sess-{n}"),
                caster: self.dto.clone(),
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
        if self.dto.id == caster_id {
            Ok(self.dto.clone())
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
// Minimal upload/waveform fakes (unused by the announcement routes)
// ---------------------------------------------------------------------------

struct NoJobs;

#[async_trait]
impl UploadJobApi for NoJobs {
    async fn create_job(&self, _j: &NewUploadJob) -> Result<String, JobError> {
        Err(JobError::not_found())
    }
    async fn patch_job(&self, _id: &str, _p: &UploadJobPatch) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn get_job(&self, _id: &str) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn list_jobs(&self, _l: i64) -> Result<Vec<UploadJob>, JobError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct NoWaveforms;

#[async_trait]
impl WaveformApi for NoWaveforms {
    async fn put_waveform(
        &self,
        _w: &zukka_portal::api_client::NewWaveform,
    ) -> Result<Waveform, WaveformApiError> {
        Err(WaveformApiError::Upstream("unused".into()))
    }
    async fn get_waveform(&self, _k: &str, _i: &str) -> Result<Option<Waveform>, WaveformApiError> {
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

/// Builds the real router with a fake identity + a fake announcement client.
/// The concrete [`FakeAnnouncementApi`] handle is returned so tests can assert
/// on the recorded creates.
fn router(perms: BTreeMap<String, bool>) -> (Router, Arc<FakeAnnouncementApi>) {
    let announcements = Arc::new(FakeAnnouncementApi::default());
    let backend = FakeScheduleBackend::new();
    let state = AppState {
        config: Arc::new(config()),
        identity: Arc::new(FakeIdentity::new(perms)),
        music: Arc::new(FakeMusicApi::new()),
        upload_jobs: Arc::new(NoJobs),
        waveforms: Arc::new(NoWaveforms),
        tracks: Arc::new(FakeTrackApi::new()),
        queues: Arc::new(FakeQueueApi::new()),
        control: Arc::new(FakeControlApi::new()),
        schedule: Arc::new(backend.schedule()),
        shows: Arc::new(backend.shows()),
        broadcasts: Arc::new(backend.broadcasts()),
        announcements: announcements.clone(),
        limiter: Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        engine: Arc::new(zukka_portal::control_ws::engine::FakeEngineClient::new()),
        meter_hub: Arc::new(zukka_portal::control_ws::meter::MeterHub::new()),
    };
    (build_router(state), announcements)
}

fn caster_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("caster")
}

fn no_perms() -> BTreeMap<String, bool> {
    BTreeMap::new()
}

// ---------------------------------------------------------------------------
// HTTP helpers (mirrors `schedule_ui_e2e.rs`)
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

async fn login(app: &Router) -> (String, String) {
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
    let session = set_cookie_value(&headers, "__Host-zukka_session").expect("session cookie");
    let (_, _, dashboard) = send(
        &app.clone(),
        Method::GET,
        "/",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    let csrf = meta_csrf(&dashboard).expect("dashboard csrf");
    (session, csrf)
}

async fn get(app: &Router, session: &str, uri: &str) -> (StatusCode, String) {
    let cookie_header = cookie(session);
    let (status, _, body) = send(app, Method::GET, uri, &[("cookie", &cookie_header)], None).await;
    (status, body)
}

async fn post(
    app: &Router,
    session: &str,
    csrf: &str,
    uri: &str,
    htmx: bool,
    body: String,
) -> (StatusCode, HeaderMap, String) {
    let cookie_header = cookie(session);
    let mut headers: Vec<(&str, &str)> = vec![
        ("cookie", &cookie_header),
        ("x-csrf-token", csrf),
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    if htmx {
        headers.push(("hx-request", "true"));
    }
    send(app, Method::POST, uri, &headers, Some(body)).await
}

// ---------------------------------------------------------------------------
// (a) GET requires `announcement.write`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn announcements_page_renders_form_with_permission() {
    let (app, _announcements) = router(caster_perms());
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(&app, &session, "/announcements").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Announcements"), "{body}");
    assert!(
        body.contains("action=\"/announcements/post\""),
        "post form rendered: {body}"
    );
    assert!(
        body.contains("name=\"caster_name\""),
        "caster field: {body}"
    );
    // The form pre-fills the session display name.
    assert!(
        body.contains("value=\"Mira\""),
        "session name prefill: {body}"
    );
}

#[tokio::test]
async fn announcements_page_without_permission_is_403() {
    let (app, _announcements) = router(no_perms());
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(&app, &session, "/announcements").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// ---------------------------------------------------------------------------
// (b) POST without `announcement.write` → 403 and no create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_without_permission_is_403_and_no_create() {
    let (app, announcements) = router(no_perms());
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/announcements/post",
        false,
        form(&[("caster_name", "Mira")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        announcements.list(None).await.unwrap().is_empty(),
        "a forbidden post must never reach the client"
    );
}

// ---------------------------------------------------------------------------
// (c) permitted post → exactly one create, caster_id from the session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_with_permission_creates_once_with_session_caster() {
    let (app, announcements) = router(caster_perms());
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/announcements/post",
        false,
        form(&[("caster_name", "Mira"), ("show_name", "Late Show")]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "classic form redirects");

    let created = announcements.list(None).await.unwrap();
    assert_eq!(created.len(), 1, "exactly one create: {created:?}");
    assert_eq!(created[0].event, "manual");
    assert_eq!(
        created[0].caster_id.as_deref(),
        Some("c-1"),
        "caster_id is the session identity, never form input"
    );
    assert_eq!(created[0].payload["caster_name"], "Mira");
    assert_eq!(created[0].payload["show_name"], "Late Show");
}

#[tokio::test]
async fn htmx_post_returns_refresh_fragment() {
    let (app, announcements) = router(caster_perms());
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/announcements/post",
        true,
        form(&[("caster_name", "Mira")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "htmx answers 204");
    assert_eq!(
        headers.get("hx-refresh").and_then(|v| v.to_str().ok()),
        Some("true"),
        "server stays the source of truth"
    );
    assert_eq!(announcements.list(None).await.unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// (c2) an unknown/empty show_name is normalised to null (blank_to_none)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn blank_show_name_is_normalised() {
    let (app, announcements) = router(caster_perms());
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/announcements/post",
        false,
        form(&[("caster_name", "Mira"), ("show_name", "   ")]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let created = announcements.list(None).await.unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].payload["show_name"], serde_json::Value::Null);
}
