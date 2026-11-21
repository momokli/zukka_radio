//! Route tests for the track waveform endpoints (Issue #18, D8).
//!
//! The scheduler is replaced by an in-memory [`FakeWaveforms`]; the auth layer
//! is bypassed by injecting an [`AuthContext`] directly, so the handlers under
//! test are the real ones (`GET /library/tracks/{id}/waveform.json|svg`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::Response;
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;

use zukka_portal::api_client::{
    CasterDto, CreatedSessionDto, FakeAnnouncementApi, FakeControlApi, FakeQueueApi, FakeTrackApi,
    IdentityApi, IdentityError, JobError, NewUploadJob, NewWaveform, SessionValidationDto,
    UploadJob, UploadJobApi, UploadJobPatch, Waveform, WaveformApi, WaveformApiError,
};
use zukka_portal::api_client::{CasterUpdate, CreatedCaster, NewCaster};
use zukka_portal::app::AppState;
use zukka_portal::auth::AuthContext;
use zukka_portal::config::Config;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

// ---------------------------------------------------------------------------
// Minimal fakes
// ---------------------------------------------------------------------------

struct NoIdentity;

#[async_trait]
impl IdentityApi for NoIdentity {
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
    async fn verify(&self, _u: &str, _p: &str) -> Result<CasterDto, IdentityError> {
        Err(IdentityError::InvalidCredentials)
    }
    async fn create_session(
        &self,
        _c: &str,
        _i: Option<&str>,
        _u: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        Err(IdentityError::NotFound)
    }
    async fn validate_session(
        &self,
        _t: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError> {
        Ok(None)
    }
    async fn revoke_session(&self, _s: &str) -> Result<(), IdentityError> {
        Ok(())
    }
    async fn get_caster(&self, _c: &str) -> Result<CasterDto, IdentityError> {
        Err(IdentityError::NotFound)
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
struct FakeWaveforms {
    rows: Mutex<Vec<Waveform>>,
}

impl FakeWaveforms {
    fn with_track(id: &str, bars: i64, data: Vec<u8>) -> Self {
        Self {
            rows: Mutex::new(vec![Waveform {
                subject_kind: "track".into(),
                subject_id: id.into(),
                scheme: 1,
                algorithm: "ffmpeg-minmax-8k/1".into(),
                ms_per_bar: 500,
                bars,
                data,
                created_at: "2026-11-18T12:00:00.000Z".into(),
            }]),
        }
    }
}

#[async_trait]
impl WaveformApi for FakeWaveforms {
    async fn put_waveform(&self, w: &NewWaveform) -> Result<Waveform, WaveformApiError> {
        Ok(Waveform {
            subject_kind: w.subject_kind.clone(),
            subject_id: w.subject_id.clone(),
            scheme: w.scheme,
            algorithm: w.algorithm.clone(),
            ms_per_bar: w.ms_per_bar,
            bars: w.bars,
            data: w.data.clone(),
            created_at: "2026-11-18T12:00:00.000Z".into(),
        })
    }
    async fn get_waveform(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.subject_kind == kind && w.subject_id == id)
            .cloned())
    }
}

// ---------------------------------------------------------------------------
// Router
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

fn ctx(role: &str) -> AuthContext {
    AuthContext {
        session_id: "s".into(),
        caster_id: "c".into(),
        username: "u".into(),
        display_name: "U".into(),
        role: role.into(),
        permissions: rbac::permissions_for_role(role),
        csrf_token: "t".into(),
    }
}

async fn inject(mut req: Request<Body>, next: Next) -> Response {
    req.extensions_mut().insert(ctx("caster"));
    next.run(req).await
}

fn state(waveforms: Arc<dyn WaveformApi>) -> AppState {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    AppState {
        config: Arc::new(config()),
        identity: Arc::new(NoIdentity),
        music: Arc::new(FakeMusicApi::new()),
        upload_jobs: Arc::new(NoJobs),
        waveforms,
        tracks: Arc::new(FakeTrackApi::new()),
        queues: Arc::new(FakeQueueApi::new()),
        control: Arc::new(FakeControlApi::new()),
        schedule: Arc::new(backend.schedule()),
        shows: Arc::new(backend.shows()),
        broadcasts: Arc::new(backend.broadcasts()),
        announcements: Arc::new(FakeAnnouncementApi::default()),
        limiter: Arc::new(WindowLimiter::new(5, Duration::from_secs(900))),
    }
}

fn router(waveforms: Arc<dyn WaveformApi>) -> Router {
    Router::new()
        .route(
            "/library/tracks/{id}/waveform.json",
            get(zukka_portal::upload::waveform_json),
        )
        .route(
            "/library/tracks/{id}/waveform.svg",
            get(zukka_portal::upload::waveform_svg),
        )
        .route_layer(from_fn(inject))
        .with_state(state(waveforms))
}

async fn get_req(app: &Router, uri: &str) -> (StatusCode, String, Option<String>) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        String::from_utf8_lossy(&bytes).to_string(),
        content_type,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn waveform_json_returns_peaks() {
    // 2 bars → 4 bytes of (min, max) pairs.
    let fake = Arc::new(FakeWaveforms::with_track("t-1", 2, vec![128, 200, 40, 255]));
    let app = router(fake);

    let (status, body, content_type) = get_req(&app, "/library/tracks/t-1/waveform.json").await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.unwrap().contains("application/json"));
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["subject_kind"], "track");
    assert_eq!(value["subject_id"], "t-1");
    assert_eq!(value["bars"], 2);
    assert_eq!(value["data"], serde_json::json!([128, 200, 40, 255]));
    // Contract: `data` is exactly `2 * bars` interleaved (min, max) bytes.
    let data_len = value["data"].as_array().unwrap().len();
    assert_eq!(data_len, 2 * value["bars"].as_i64().unwrap() as usize);
}

#[tokio::test]
async fn waveform_json_unknown_is_404() {
    let fake = Arc::new(FakeWaveforms::default());
    let app = router(fake);
    let (status, _, _) = get_req(&app, "/library/tracks/missing/waveform.json").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn waveform_svg_renders_inline_svg() {
    let fake = Arc::new(FakeWaveforms::with_track("t-1", 2, vec![128, 200, 40, 255]));
    let app = router(fake);

    let (status, body, content_type) = get_req(&app, "/library/tracks/t-1/waveform.svg").await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.unwrap().contains("image/svg+xml"));
    assert!(body.contains("<svg"), "{body}");
    assert!(body.contains("class=\"waveform-bar\""), "{body}");
    // Two bars → two <rect class="waveform-bar">.
    assert_eq!(body.matches("waveform-bar").count(), 2, "{body}");
}

#[tokio::test]
async fn waveform_svg_unknown_is_404() {
    let fake = Arc::new(FakeWaveforms::default());
    let app = router(fake);
    let (status, _, _) = get_req(&app, "/library/tracks/missing/waveform.svg").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn waveform_js_is_served() {
    // The renderer asset lives on the unprotected static layer (like
    // `sortable.min.js`); serve it through the real app router.
    let fake = Arc::new(FakeWaveforms::default());
    let app = zukka_portal::app::build_router(state(fake));

    let (status, body, content_type) = get_req(&app, "/static/waveform.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        content_type.as_deref(),
        Some("text/javascript; charset=utf-8")
    );
    assert!(
        body.contains("IntersectionObserver"),
        "renderer entry point"
    );
    assert!(body.contains(".waveform-canvas"), "selects the canvas hook");
    assert!(body.contains("htmx:afterSwap"), "re-scans after htmx swaps");
}

#[test]
fn waveform_template_is_symmetric_shape() {
    use askama::Template;
    use zukka_portal::templates::WaveformSvg;
    let html = WaveformSvg {
        bars: 1,
        peaks: vec![10, 250],
    }
    .render()
    .unwrap();
    assert!(html.contains("viewBox=\"0 0 1 256\""), "{html}");
    assert!(html.contains("y=\"10\""), "{html}");
}
