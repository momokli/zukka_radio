//! End-to-end tests for the library (Issue #19: US-4…US-7) through the real
//! portal router.
//!
//! The scheduler is replaced by an in-memory [`FakeTrackApi`] (which enforces
//! the same §1 visibility scope); the music/job/waveform APIs are inert fakes.
//! Login uses the real session cookie + CSRF flow.

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
    CasterDto, CreatedSessionDto, FakeTrackApi, IdentityApi, IdentityError, JobError, NewUploadJob,
    NewWaveform, SessionValidationDto, Track, UploadJob, UploadJobApi, UploadJobPatch, Waveform,
    WaveformApi, WaveformApiError,
};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

// ---------------------------------------------------------------------------
// Fake identity (one caster whose permission map is configurable)
// ---------------------------------------------------------------------------

struct FakeCaster {
    dto: CasterDto,
}

struct FakeIdentity {
    casters: Mutex<Vec<FakeCaster>>,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new(perms: BTreeMap<String, bool>) -> Self {
        let dto = CasterDto {
            id: "c-1".into(),
            username: "mira".into(),
            display_name: "Mira".into(),
            email: None,
            role: "caster".into(),
            is_active: true,
            harbor_mount: None,
            stream_username: None,
            permissions: perms,
        };
        Self {
            casters: Mutex::new(vec![FakeCaster { dto }]),
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.username == username && password == "pw")
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::InvalidCredentials)
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
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: format!("sess-{n}"),
                caster,
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
// Inert job/waveform fakes
// ---------------------------------------------------------------------------

#[derive(Default)]
struct NoJobs;

#[async_trait]
impl UploadJobApi for NoJobs {
    async fn create_job(&self, _job: &NewUploadJob) -> Result<String, JobError> {
        Err(JobError::not_found())
    }
    async fn patch_job(&self, _id: &str, _p: &UploadJobPatch) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn get_job(&self, _id: &str) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn list_jobs(&self, _limit: i64) -> Result<Vec<UploadJob>, JobError> {
        Ok(vec![])
    }
}

#[derive(Default)]
struct NoWaveforms;

#[async_trait]
impl WaveformApi for NoWaveforms {
    async fn put_waveform(&self, _w: &NewWaveform) -> Result<Waveform, WaveformApiError> {
        Err(WaveformApiError::Upstream("noop".into()))
    }
    async fn get_waveform(
        &self,
        _k: &str,
        _id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError> {
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

fn router(perms: BTreeMap<String, bool>, tracks: Arc<FakeTrackApi>) -> Router {
    let state = AppState::with_clients(
        config(),
        Arc::new(FakeIdentity::new(perms)),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(FakeMusicApi::new()),
        Arc::new(NoJobs),
        Arc::new(NoWaveforms),
        tracks,
    );
    build_router(state)
}

fn caster_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("caster")
}

fn operator_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("operator")
}

fn track(id: &str, title: &str, visibility: &str, caster: Option<&str>) -> Track {
    Track {
        id: id.into(),
        music_api_id: format!("m-{id}"),
        title: title.into(),
        artist: "Artist".into(),
        album: "Album".into(),
        duration_ms: 1200,
        storage_key: format!("key/{id}"),
        artwork_url: None,
        visibility: visibility.into(),
        caster_id: caster.map(str::to_string),
        created_at: "2026-11-18T12:00:00.000Z".into(),
        updated_at: "2026-11-18T12:00:00.000Z".into(),
    }
}

fn seeded() -> Arc<FakeTrackApi> {
    let api = FakeTrackApi::new();
    api.seed(track("t-shared", "Shared Song", "shared", Some("c-2")));
    api.seed(track("t-mine", "Mine Private", "private", Some("c-1")));
    api.seed(track("t-foreign", "Foreign Secret", "private", Some("c-2")));
    Arc::new(api)
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

/// Logs in as `mira` / `pw`; returns `(session_cookie, csrf_token)`.
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
        app,
        Method::GET,
        "/",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    let csrf = meta_csrf(&dashboard).expect("dashboard csrf");
    (session, csrf)
}

// ---------------------------------------------------------------------------
// US-4: list / search / fragment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn library_lists_visible_tracks() {
    let app = router(caster_perms(), seeded());
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<html"), "full page rendered");
    assert!(body.contains("id=\"library-results\""), "{body}");
    assert!(body.contains("Shared Song"));
    assert!(body.contains("Mine Private"));
    assert!(!body.contains("Foreign Secret"), "foreign private hidden");
    assert!(
        body.contains("hx-post=\"/library/upload\""),
        "upload block stays"
    );
}

#[tokio::test]
async fn library_htmx_returns_fragment_only() {
    let app = router(caster_perms(), seeded());
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library",
        &[("cookie", &cookie(&session)), ("hx-request", "true")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("<html"), "fragment only: {body}");
    assert!(body.contains("Shared Song"));
    assert!(body.contains("badge"));
}

#[tokio::test]
async fn library_private_filter_shows_only_own() {
    let app = router(caster_perms(), seeded());
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library?visibility=private",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Mine Private"));
    assert!(!body.contains("Foreign Secret"));
    assert!(!body.contains("Shared Song"), "shared filtered out");
}

#[tokio::test]
async fn library_search_hides_foreign_private() {
    let app = router(caster_perms(), seeded());
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library?q=Foreign",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Keine Treffer"),
        "no hit for foreign private: {body}"
    );
}

#[tokio::test]
async fn library_empty_state() {
    let app = router(caster_perms(), Arc::new(FakeTrackApi::new()));
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Library leer"), "{body}");
}

// ---------------------------------------------------------------------------
// US-5: detail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn track_detail_renders_waveform_and_metadata() {
    let app = router(caster_perms(), seeded());
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library/tracks/t-mine",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("/library/tracks/t-mine/waveform.svg"));
    assert!(body.contains("Mine Private"));
    assert!(body.contains("1200"));
}

#[tokio::test]
async fn foreign_private_detail_is_404() {
    let app = router(caster_perms(), seeded());
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/library/tracks/t-foreign",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!body.contains("Foreign Secret"));
}

// ---------------------------------------------------------------------------
// US-6: visibility toggle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn visibility_toggle_roundtrip() {
    let tracks = seeded();
    let app = router(caster_perms(), tracks.clone());
    let (session, csrf) = login(&app).await;

    // private → shared via htmx: updated row fragment.
    let (status, _, body) = send(
        &app,
        Method::POST,
        "/library/tracks/t-mine/visibility",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
            ("hx-request", "true"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(form(&[("visibility", "shared")])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("id=\"track-t-mine\""), "{body}");
    assert_eq!(
        tracks
            .all()
            .iter()
            .find(|t| t.id == "t-mine")
            .unwrap()
            .visibility,
        "shared"
    );

    // shared → private again (non-htmx → redirect).
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-mine/visibility",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(form(&[("visibility", "private")])),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "non-htmx redirects");
    assert_eq!(
        tracks
            .all()
            .iter()
            .find(|t| t.id == "t-mine")
            .unwrap()
            .visibility,
        "private"
    );
}

#[tokio::test]
async fn visibility_toggle_requires_permission_and_csrf() {
    let mut perms = caster_perms();
    perms.insert("library.upload".into(), false);
    let tracks = seeded();
    let app = router(perms, tracks.clone());
    let (session, csrf) = login(&app).await;

    // RBAC blocks without `library.upload` → 403.
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-mine/visibility",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(form(&[("visibility", "shared")])),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Missing CSRF token → 403 (CSRF layer runs first).
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-mine/visibility",
        &[
            ("cookie", &cookie(&session)),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(form(&[("visibility", "shared")])),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn visibility_toggle_foreign_shared_is_404() {
    // The logged-in caster (c-1) can see `t-shared` (owned by c-2) but must not
    // toggle it: the scheduler PATCH is owner-only and answers 404.
    let tracks = seeded();
    let app = router(caster_perms(), tracks.clone());
    let (session, csrf) = login(&app).await;

    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-shared/visibility",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(form(&[("visibility", "private")])),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        tracks
            .all()
            .iter()
            .find(|t| t.id == "t-shared")
            .unwrap()
            .visibility,
        "shared",
        "foreign shared track unchanged"
    );
}

// ---------------------------------------------------------------------------
// US-7: delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_requires_delete_permission() {
    let tracks = seeded();
    // The caster has `library.read`/`upload` but not `library.delete`.
    let app = router(caster_perms(), tracks.clone());
    let (session, csrf) = login(&app).await;

    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-mine/delete",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(tracks.all().len(), 3, "nothing deleted");
}

#[tokio::test]
async fn operator_can_delete_track() {
    let tracks = seeded();
    let app = router(operator_perms(), tracks.clone());
    let (session, csrf) = login(&app).await;

    let (status, headers, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-shared/delete",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
            ("hx-request", "true"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("hx-redirect").unwrap(), "/library");
    assert!(tracks.all().iter().all(|t| t.id != "t-shared"));
}

#[tokio::test]
async fn delete_foreign_private_is_404() {
    let tracks = seeded();
    let app = router(operator_perms(), tracks.clone());
    let (session, csrf) = login(&app).await;

    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/t-foreign/delete",
        &[
            ("cookie", &cookie(&session)),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(tracks.all().iter().any(|t| t.id == "t-foreign"));
}
