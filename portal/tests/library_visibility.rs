//! Portal visibility matrix for the library (Issue #19, §1 core acceptance).
//!
//! Two casters (mira → `c-1`, zoe → `c-2`) share a library. The rule under
//! test:
//!
//! ```text
//! visible(track, viewer) := track.visibility = 'shared' OR track.caster_id = viewer
//! ```
//!
//! A foreign private track must be invisible in the list, in the search **and**
//! via a direct URL (`404`, never `403` — existence is not revealed).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use zukka_portal::api_client::{
    CasterDto, CreatedSessionDto, FakeControlApi, FakeQueueApi, FakeTrackApi, IdentityApi,
    IdentityError, JobError, NewUploadJob, NewWaveform, SessionValidationDto, Track, UploadJob,
    UploadJobApi, UploadJobPatch, Waveform, WaveformApi, WaveformApiError,
};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

const PASSWORD: &str = "pw";

struct FakeIdentity {
    casters: Vec<CasterDto>,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new() -> Self {
        let casters = [("c-1", "mira"), ("c-2", "zoe")]
            .into_iter()
            .map(|(id, username)| CasterDto {
                id: id.into(),
                username: username.into(),
                display_name: username.to_string(),
                email: None,
                role: "caster".into(),
                is_active: true,
                harbor_mount: None,
                stream_username: None,
                permissions: rbac::permissions_for_role("caster"),
            })
            .collect();
        Self {
            casters,
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .iter()
            .find(|c| c.username == username && password == PASSWORD)
            .cloned()
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
            .iter()
            .find(|c| c.id == caster_id)
            .cloned()
            .ok_or(IdentityError::NotFound)?;
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: format!("sess-{n}"),
                caster,
                csrf_token: format!("csrf-{n}"),
                idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
                absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
            },
        );
        Ok(CreatedSessionDto {
            id: format!("sess-{n}"),
            token,
            csrf_token: format!("csrf-{n}"),
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
    async fn revoke_session(&self, _id: &str) -> Result<(), IdentityError> {
        Ok(())
    }
    async fn get_caster(&self, id: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .iter()
            .find(|c| c.id == id)
            .cloned()
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

#[derive(Default)]
struct NoJobs;
#[async_trait]
impl UploadJobApi for NoJobs {
    async fn create_job(&self, _j: &NewUploadJob) -> Result<String, JobError> {
        Err(JobError::not_found())
    }
    async fn patch_job(&self, _i: &str, _p: &UploadJobPatch) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn get_job(&self, _i: &str) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn list_jobs(&self, _l: i64) -> Result<Vec<UploadJob>, JobError> {
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
    async fn get_waveform(&self, _k: &str, _i: &str) -> Result<Option<Waveform>, WaveformApiError> {
        Ok(None)
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

fn track(id: &str, title: &str, visibility: &str, caster: &str) -> Track {
    Track {
        id: id.into(),
        music_api_id: format!("m-{id}"),
        title: title.into(),
        artist: "Artist".into(),
        album: String::new(),
        duration_ms: 1000,
        storage_key: format!("key/{id}"),
        artwork_url: None,
        visibility: visibility.into(),
        caster_id: Some(caster.into()),
        created_at: "2026-11-18T12:00:00.000Z".into(),
        updated_at: "2026-11-18T12:00:00.000Z".into(),
    }
}

fn app(tracks: Arc<FakeTrackApi>) -> Router {
    build_router(AppState::with_clients(
        config(),
        Arc::new(FakeIdentity::new()),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(FakeMusicApi::new()),
        Arc::new(NoJobs),
        Arc::new(NoWaveforms),
        tracks,
        Arc::new(FakeQueueApi::new()),
        Arc::new(FakeControlApi::new()),
    ))
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
        Some(b) => builder.body(Body::from(b)).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

fn set_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(axum::http::header::SET_COOKIE) {
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

async fn login(app: &Router, username: &str) -> String {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let login_cookie = set_cookie(&headers, "__Host-zukka_csrf").unwrap();
    let token = meta_csrf(&body).unwrap();
    let payload = format!("username={username}&password={PASSWORD}&csrf_token={token}");
    let (status, headers, _) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={login_cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    set_cookie(&headers, "__Host-zukka_session").expect("session")
}

fn seeded() -> Arc<FakeTrackApi> {
    let api = FakeTrackApi::new();
    api.seed(track("shared", "Public Track", "shared", "c-2"));
    api.seed(track("mine", "Mine Secret", "private", "c-1"));
    api.seed(track("foreign", "Foreign Secret", "private", "c-2"));
    Arc::new(api)
}

/// §1 matrix: list / search / direct URL for both viewers.
#[tokio::test]
async fn visibility_matrix_two_viewers() {
    let app = app(seeded());
    let mira = login(&app, "mira").await;
    let zoe = login(&app, "zoe").await;

    for (viewer, session, secret) in [
        ("mira", &mira, "Foreign Secret"),
        ("zoe", &zoe, "Mine Secret"),
    ] {
        let ck = format!("__Host-zukka_session={session}");

        // List: shared + own private; the other's private is absent.
        let (status, _, body) = send(&app, Method::GET, "/library", &[("cookie", &ck)], None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Public Track"), "{viewer} sees shared");
        assert!(!body.contains(secret), "{viewer} must not see {secret}");

        // Search by the foreign private title → no hit.
        let q = secret.replace(' ', "%20");
        let (status, _, body) = send(
            &app,
            Method::GET,
            &format!("/library?q={q}"),
            &[("cookie", &ck)],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Keine Treffer"),
            "{viewer} search must miss {secret}"
        );
    }

    let mira_ck = format!("__Host-zukka_session={mira}");

    // Direct URL: foreign private → 404; own private → 200; shared → 200.
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/library/tracks/foreign",
        &[("cookie", &mira_ck)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign private → 404");

    let (status, _, _) = send(
        &app,
        Method::GET,
        "/library/tracks/mine",
        &[("cookie", &mira_ck)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "own private → 200");

    let (status, _, _) = send(
        &app,
        Method::GET,
        "/library/tracks/shared",
        &[("cookie", &mira_ck)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "shared → 200");
}

/// After the owner flips a track to private it disappears for the other viewer.
#[tokio::test]
async fn owner_toggle_hides_from_other_viewer() {
    let app = app(seeded());
    let mira = login(&app, "mira").await;
    let zoe = login(&app, "zoe").await;
    let mira_ck = format!("__Host-zukka_session={mira}");
    let zoe_ck = format!("__Host-zukka_session={zoe}");

    // mira sees the shared track.
    let (_, _, body) = send(&app, Method::GET, "/library", &[("cookie", &mira_ck)], None).await;
    assert!(body.contains("Public Track"));

    // The owner (zoe) makes it private.
    let (status, _, dashboard) = send(&app, Method::GET, "/", &[("cookie", &zoe_ck)], None).await;
    assert_eq!(status, StatusCode::OK);
    let csrf = meta_csrf(&dashboard).unwrap();
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/tracks/shared/visibility",
        &[
            ("cookie", &zoe_ck),
            ("x-csrf-token", &csrf),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some("visibility=private".into()),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // mira (other viewer) no longer sees it, in list or by URL.
    let (_, _, body) = send(&app, Method::GET, "/library", &[("cookie", &mira_ck)], None).await;
    assert!(!body.contains("Public Track"), "now private");
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/library/tracks/shared",
        &[("cookie", &mira_ck)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
