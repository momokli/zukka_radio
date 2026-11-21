//! End-to-end tests for the Control Panel (Issue #22, US-3…US-6) through the
//! real portal router with an in-memory [`FakeControlApi`].
//!
//! Explicit proofs:
//!   (a) `caster` (no `control.write`) sees no action buttons and gets `403`
//!       on `POST /control/*`; `operator` gets the buttons,
//!   (b) the `control-state` fragment renders layers, now-playing and the
//!       empty-`connected` state (O2), and a scheduler outage degrades to a
//!       hint instead of a `500` (D20),
//!   (c) `POST /control/source` returns an `HX-Trigger` toast with an undo
//!       payload carrying the previous values (K5),
//!   (d) the pause live-guard surfaces an informational toast, not an error.

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
    CasterDto, ConnectedCaster, ControlSource, ControlState, CreatedSessionDto, FakeControlApi,
    FakeQueueApi, FakeTrackApi, IdentityApi, IdentityError, JobError, LiveState, NewUploadJob,
    NewWaveform, NowPlaying, ObservedNowPlaying, SessionValidationDto, UploadJob, UploadJobApi,
    UploadJobPatch, Waveform, WaveformApi, WaveformApiError,
};
use zukka_portal::api_client::{CasterUpdate, CreatedCaster, NewCaster};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;
use zukka_portal::templates::ControlView;

// ---------------------------------------------------------------------------
// Fake identity
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
// Inert fakes
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

fn router(perms: BTreeMap<String, bool>, control: Arc<FakeControlApi>) -> Router {
    let state = AppState::with_clients(
        config(),
        Arc::new(FakeIdentity::new(perms)),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(FakeMusicApi::new()),
        Arc::new(NoJobs),
        Arc::new(NoWaveforms),
        Arc::new(FakeTrackApi::new()),
        Arc::new(FakeQueueApi::new()),
        control,
    );
    build_router(state)
}

fn caster_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("caster")
}

fn operator_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("operator")
}

fn sample_state(active_caster: Option<&str>) -> ControlState {
    ControlState {
        sources: vec![
            ControlSource {
                id: "s-live".into(),
                name: "live".into(),
                kind: "live".into(),
                priority: 100,
                enabled: true,
            },
            ControlSource {
                id: "s-queue".into(),
                name: "queue".into(),
                kind: "queue".into(),
                priority: 60,
                enabled: true,
            },
            ControlSource {
                id: "s-fun".into(),
                name: "fun".into(),
                kind: "fun".into(),
                priority: 1,
                enabled: false,
            },
        ],
        paused: false,
        forced_layer: None,
        active_layer: Some("queue".into()),
        derived: true,
        live: LiveState {
            override_caster_id: None,
            active_caster_id: active_caster.map(str::to_string),
            connected: vec![ConnectedCaster {
                caster_id: "c-1".into(),
                mount: Some("/live/mira".into()),
                since: "2026-11-19T12:00:00.000Z".into(),
            }],
        },
        now_playing: Some(NowPlaying {
            queue_item_id: Some("qi-1".into()),
            kind: "track".into(),
            title: "Nachtfahrt".into(),
            artist: Some("Kranich".into()),
            duration_ms: 210_000,
        }),
        // Issue #140: Icecast observation fields.
        observed_layer: Some("queue".into()),
        derived_layer: Some("queue".into()),
        active_layer_source: Some("observed".into()),
        observed: true,
        observed_reason: Some("db_match".into()),
        on_air: true,
        observed_now_playing: None,
    }
}

#[test]
fn observed_relay_and_silence_render() {
    let mut state = sample_state(None);
    state.active_layer = Some("relay".into());
    state.observed_layer = Some("relay".into());
    state.derived_layer = None;
    state.active_layer_source = Some("observed".into());
    state.observed = true;
    state.observed_reason = Some("external".into());
    state.on_air = true;
    state.now_playing = None;
    state.observed_now_playing = Some(ObservedNowPlaying {
        title: Some("Deep Space".into()),
        artist: Some("SomaFM".into()),
        source: Some("Icecast".into()),
    });
    let view = ControlView::from_state(state, false);
    assert_eq!(view.active_layer.as_deref(), Some("relay"));
    assert_eq!(view.on_air_label, "relay");
    assert_eq!(view.layer_source.as_deref(), Some("beobachtet"));
    assert_eq!(view.now_title.as_deref(), Some("Deep Space"));
    assert_eq!(view.now_source.as_deref(), Some("Icecast"));

    let mut silent = sample_state(None);
    silent.active_layer = None;
    silent.observed_layer = None;
    silent.derived_layer = None;
    silent.active_layer_source = None;
    silent.observed = false;
    silent.observed_reason = Some("silent".into());
    silent.on_air = false;
    silent.now_playing = None;
    silent.observed_now_playing = None;
    let view = ControlView::from_state(silent, false);
    assert_eq!(view.on_air_label, "Stille");
    assert!(view.on_air_note.is_none());
    assert!(view.active_layer.is_none());
    assert!(view.now_title.is_none());
}

#[test]
fn derived_layer_labels_as_abgeleitet() {
    let mut state = sample_state(None);
    state.active_layer_source = Some("derived".into());
    state.observed_layer = None;
    state.derived_layer = Some("queue".into());
    state.observed = false;
    let view = ControlView::from_state(state, false);
    assert_eq!(view.layer_source.as_deref(), Some("abgeleitet"));
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

async fn get(app: &Router, session: &str, uri: &str, htmx: bool) -> (StatusCode, String) {
    let cookie_header = cookie(session);
    let mut headers: Vec<(&str, &str)> = vec![("cookie", &cookie_header)];
    if htmx {
        headers.push(("hx-request", "true"));
    }
    let (status, _, body) = send(app, Method::GET, uri, &headers, None).await;
    (status, body)
}

// ---------------------------------------------------------------------------
// Proof (a): read-only vs. write capability
// ---------------------------------------------------------------------------

#[tokio::test]
async fn caster_panel_is_read_only() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(Some("c-1")));
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;

    let (status, body) = get(&app, &session, "/control", false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Control Panel"), "{body}");
    assert!(body.contains("Nachtfahrt"), "now playing rendered");
    assert!(!body.contains("action=\"/control/source\""), "no actions");
    assert!(!body.contains("action=\"/control/pause\""), "no pause");
}

#[tokio::test]
async fn operator_panel_has_actions() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(operator_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control", false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("action=\"/control/source\""));
    assert!(body.contains("action=\"/control/pause\""));
    assert!(body.contains("action=\"/control/skip\""));
}

#[tokio::test]
async fn caster_source_is_403() {
    let app = router(caster_perms(), Arc::new(FakeControlApi::new()));
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/control/source",
        true,
        form(&[("target_kind", "layer"), ("target", "queue")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Proof (b): fragment content + outage degradation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_fragment_renders_content() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("<html"), "fragment only");
    assert!(body.contains("id=\"control-state\""), "{body}");
    assert!(body.contains("Quellen"));
    assert!(body.contains("badge-active"), "active layer marked");
    assert!(!body.contains("Keine verbundenen Caster."));
    assert!(body.contains("/live/mira"), "connected caster + mount");
    assert!(body.contains("3:30"), "duration formatted");
}

/// Issue #140: the fragment shows the effective layer with its observation
/// origin ("beobachtet").
#[tokio::test]
async fn fragment_shows_observed_layer_origin() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("On Air:"), "{body}");
    assert!(body.contains("beobachtet"), "observation origin: {body}");
    assert!(body.contains("Datenbank"), "DB now-playing origin: {body}");
}

/// Issue #140: a relay seen via ICY metadata is named, and the observed
/// now-playing fills the gap left by the DB path.
#[tokio::test]
async fn fragment_shows_relay_from_icecast() {
    let control = Arc::new(FakeControlApi::new());
    let mut state = sample_state(None);
    state.active_layer = Some("relay".into());
    state.observed_layer = Some("relay".into());
    state.derived_layer = None;
    state.active_layer_source = Some("observed".into());
    state.observed = true;
    state.observed_reason = Some("external".into());
    state.on_air = true;
    state.now_playing = None;
    state.observed_now_playing = Some(ObservedNowPlaying {
        title: Some("Deep Space".into()),
        artist: Some("SomaFM".into()),
        source: Some("Icecast".into()),
    });
    control.set_state(state);
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("relay"), "relay named: {body}");
    assert!(body.contains("SomaFM"), "observed artist: {body}");
    assert!(body.contains("Deep Space"), "observed title: {body}");
    assert!(body.contains("Icecast"), "observed origin: {body}");
}

/// Issue #140: no source on air renders "Stille".
#[tokio::test]
async fn fragment_shows_silence() {
    let control = Arc::new(FakeControlApi::new());
    let mut state = sample_state(None);
    state.active_layer = None;
    state.observed_layer = None;
    state.derived_layer = None;
    state.active_layer_source = None;
    state.observed = false;
    state.observed_reason = Some("silent".into());
    state.on_air = false;
    state.now_playing = None;
    state.observed_now_playing = None;
    control.set_state(state);
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Stille"), "{body}");
    assert!(body.contains("Nichts läuft."), "{body}");
}

/// Issue #140 / US7: a silent Icecast with a non-null derived layer still
/// renders "Stille" (the observed signal wins over the derived intent).
#[tokio::test]
async fn fragment_shows_silence_despite_derived_layer() {
    let control = Arc::new(FakeControlApi::new());
    let mut state = sample_state(None);
    // Effective layer stays on the derived one, but Icecast is silent.
    state.active_layer = Some("rotation".into());
    state.observed_layer = None;
    state.derived_layer = Some("rotation".into());
    state.active_layer_source = Some("derived".into());
    state.observed = false;
    state.observed_reason = Some("silent".into());
    state.on_air = false;
    state.now_playing = None;
    state.observed_now_playing = None;
    control.set_state(state);
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Stille"), "silence despite derived: {body}");
    assert!(
        body.contains("Geplant (abgeleitet): <code>rotation</code>"),
        "derived intent still visible: {body}"
    );
}

/// Issue #140: an unreachable Icecast renders "unbekannt", never "Stille".
#[tokio::test]
async fn fragment_shows_unknown_when_icecast_unreachable() {
    let control = Arc::new(FakeControlApi::new());
    let mut state = sample_state(None);
    state.active_layer = Some("queue".into());
    state.observed_layer = None;
    state.derived_layer = Some("queue".into());
    state.active_layer_source = Some("derived".into());
    state.observed = false;
    state.observed_reason = Some("icecast_unreachable".into());
    state.on_air = false;
    state.now_playing = None;
    state.observed_now_playing = None;
    control.set_state(state);
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("unbekannt"), "{body}");
    assert!(
        body.contains("Icecast nicht erreichbar"),
        "explains the outage: {body}"
    );
    assert!(!body.contains("Stille"), "not silent: {body}");
}

#[tokio::test]
async fn empty_connected_is_acceptance_state() {
    let control = Arc::new(FakeControlApi::new());
    let mut state = sample_state(None);
    state.live.connected = vec![];
    control.set_state(state);
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Keine verbundenen Caster."), "{body}");
}

#[tokio::test]
async fn scheduler_outage_degrades_to_hint() {
    let control = Arc::new(FakeControlApi::new());
    control.fail_state();
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "no 500 on outage: {body}");
    assert!(body.contains("nicht erreichbar"), "{body}");
}

// ---------------------------------------------------------------------------
// Proof (c): source switch + undo payload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn switch_returns_undo_toast() {
    let control = Arc::new(FakeControlApi::new());
    let mut prev = sample_state(None);
    prev.active_layer = Some("rotation".into());
    control.set_state(prev);
    let app = router(operator_perms(), control.clone());
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/control/source",
        true,
        form(&[
            ("target_kind", "layer"),
            ("target", "queue"),
            ("reason", "Handover"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let trigger = headers.get("hx-trigger").unwrap().to_str().unwrap();
    assert!(trigger.contains("toast"), "{trigger}");
    assert!(trigger.contains("undo"), "{trigger}");
    assert!(
        trigger.contains("rotation"),
        "undo carries previous layer: {trigger}"
    );
    assert_eq!(
        control.switches(),
        vec![("queue".to_string(), "Handover".to_string())]
    );
}

#[tokio::test]
async fn caster_override_clears_via_caster_target() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(operator_perms(), control.clone());
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/control/source",
        true,
        form(&[
            ("target_kind", "caster"),
            ("target", ""),
            ("reason", "clear"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(control.lives(), vec![(None, "clear".to_string())]);
}

// ---------------------------------------------------------------------------
// Proof (d): pause live guard + skip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pause_live_active_is_info_toast() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(Some("c-1")));
    control.fail_pause_live();
    let app = router(operator_perms(), control);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/control/pause",
        true,
        form(&[("paused", "true"), ("reason", "x")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let trigger = headers.get("hx-trigger").unwrap().to_str().unwrap();
    assert!(trigger.contains("Live aktiv"), "{trigger}");
    assert!(trigger.contains("\"info\""), "{trigger}");
}

#[tokio::test]
async fn skip_records_reason() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(operator_perms(), control.clone());
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/control/skip",
        true,
        form(&[("reason", "weg damit")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(control.calls(), vec!["weg damit".to_string()]);
}

// ---------------------------------------------------------------------------
// Live badge (US-4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn badge_shows_own_live_status() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(Some("c-1")));
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (_, body) = get(&app, &session, "/control/badge", true).await;
    assert!(body.contains("du bist live"), "{body}");
    assert!(body.contains("aria-live=\"polite\""), "{body}");
}

#[tokio::test]
async fn badge_hidden_when_not_live() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(Some("someone-else")));
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (_, body) = get(&app, &session, "/control/badge", true).await;
    assert!(body.contains("nicht live"), "{body}");
    assert!(!body.contains("du bist live"), "{body}");
}

// ---------------------------------------------------------------------------
// Structural proofs: SSE route, polling fallback, live-guarded pause, undo
// ---------------------------------------------------------------------------

/// Like [`send`], but never collects the body — required for the SSE endpoint,
/// whose stream does not end. Only status + headers are inspected.
async fn send_headers(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).unwrap();
    let response = app.clone().oneshot(request).await.expect("response");
    (response.status(), response.headers().clone())
}

/// `GET /control/events` is auth-gated (unauthenticated GET redirects to
/// `/login`) and, when authenticated, answers `text/event-stream` (D15).
#[tokio::test]
async fn sse_route_is_event_stream_and_auth_gated() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(caster_perms(), control);

    let (status, headers) = send_headers(&app, Method::GET, "/control/events", &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "auth-gated: unauthenticated");
    assert_eq!(
        headers.get(header::LOCATION).unwrap().to_str().unwrap(),
        "/login"
    );

    let (session, _) = login(&app).await;
    let cookie_header = cookie(&session);
    let (status, headers) = send_headers(
        &app,
        Method::GET,
        "/control/events",
        &[("cookie", &cookie_header)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "authenticated SSE");
    let ct = headers
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .unwrap();
    assert!(ct.starts_with("text/event-stream"), "content-type {ct}");
}

/// The panel container wires the SSE extension **and** the `every 3s` polling
/// fallback (D15); the undo-toast window is 10 s (D16/K5).
#[tokio::test]
async fn panel_wires_sse_and_polling_fallback() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(None));
    let app = router(caster_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control", false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("sse-connect=\"/control/events\""), "{body}");
    assert!(body.contains("every 3s"), "polling fallback: {body}");
    assert!(body.contains("hx-get=\"/control/state\""), "{body}");
    // undo toast window (10 s) rather than the 4 s info toast
    assert!(body.contains("10000"), "undo 10 s window: {body}");
}

/// When live is active the pause control is `disabled` **and** explained
/// (acceptance §7: "Pause ist für Live-Quellen deaktiviert und erklärt das im UI").
#[tokio::test]
async fn pause_is_disabled_and_explained_when_live() {
    let control = Arc::new(FakeControlApi::new());
    control.set_state(sample_state(Some("c-1")));
    let app = router(operator_perms(), control);
    let (session, _) = login(&app).await;
    let (status, body) = get(&app, &session, "/control/state", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("disabled>Pause"), "pause disabled: {body}");
    assert!(
        body.contains("Live aktiv \u{2014} Pause nicht m\u{f6}glich."),
        "live explanation: {body}"
    );
}

/// The undo payload is a real, round-trippable request: reposting the previous
/// layer restores it (D16/K5) — no server-side undo store needed.
#[tokio::test]
async fn undo_repost_restores_previous_layer() {
    let control = Arc::new(FakeControlApi::new());
    let mut prev = sample_state(None);
    prev.active_layer = Some("rotation".into());
    control.set_state(prev);
    let app = router(operator_perms(), control.clone());
    let (session, csrf) = login(&app).await;

    // switch away from `rotation` to `queue`
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/control/source",
        true,
        form(&[("target_kind", "layer"), ("target", "queue")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let trigger = headers.get("hx-trigger").unwrap().to_str().unwrap();
    assert!(trigger.contains("rotation"), "undo carries previous");

    // repost the undo payload: back to `rotation`
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/control/source",
        true,
        form(&[
            ("target_kind", "layer"),
            ("target", "rotation"),
            ("reason", "R\u{fc}ckg\u{e4}ngig"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let calls = control.switches();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "queue");
    assert_eq!(calls[1].0, "rotation", "previous source restored");
}
