//! Application state + router.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::header;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::api_client::{
    AnnouncementApi, BroadcastApi, ControlApi, HttpAnnouncementApi, HttpBroadcastApi,
    HttpControlApi, HttpQueueApi, HttpScheduleApi, HttpShowApi, HttpTrackApi, HttpUploadJobApi,
    HttpWaveformApi, IdentityApi, QueueApi, ScheduleApi, ShowApi, TrackApi, UploadJobApi,
    WaveformApi,
};
use crate::auth::{handlers, middleware};
use crate::config::Config;
use crate::control_ws::engine::{EngineClient, LiquidsoapEngineClient};
use crate::control_ws::meter::MeterHub;
use crate::error::PortalError;
use crate::music_api::{HttpMusicApi, MusicApi};
use crate::rate_limit::WindowLimiter;
use crate::upload;

/// Login rate limit: 5 failures / 15 min (portal.md D3).
pub const LOGIN_MAX_ATTEMPTS: usize = 5;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

const HTMX_JS: &str = include_str!("../static/htmx.min.js");
const HTMX_SSE_JS: &str = include_str!("../static/htmx-ext-sse.js");
const APP_CSS: &str = include_str!("../static/app.css");
const QUEUE_CSS: &str = include_str!("../static/queue.css");
const SORTABLE_JS: &str = include_str!("../static/sortable.min.js");
const WAVEFORM_JS: &str = include_str!("../static/waveform.js");

// Vendored fonts (docs/portal.md D18: no CDN). Served unprotected so the
// `@font-face` rules in `queue.css` resolve without a session.
const VT323_WOFF2: &[u8] = include_bytes!("../static/fonts/VT323.woff2");
const SPACE_MONO_REGULAR_WOFF2: &[u8] = include_bytes!("../static/fonts/SpaceMono-Regular.woff2");
const SPACE_MONO_BOLD_WOFF2: &[u8] = include_bytes!("../static/fonts/SpaceMono-Bold.woff2");

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub identity: Arc<dyn IdentityApi>,
    pub music: Arc<dyn MusicApi>,
    pub upload_jobs: Arc<dyn UploadJobApi>,
    pub waveforms: Arc<dyn WaveformApi>,
    pub tracks: Arc<dyn TrackApi>,
    pub queues: Arc<dyn QueueApi>,
    pub control: Arc<dyn ControlApi>,
    pub schedule: Arc<dyn ScheduleApi>,
    pub shows: Arc<dyn ShowApi>,
    pub broadcasts: Arc<dyn BroadcastApi>,
    pub announcements: Arc<dyn AnnouncementApi>,
    pub limiter: Arc<WindowLimiter>,
    /// Liquidsoap command client for the cockpit WebSocket bridge (#148).
    pub engine: Arc<dyn EngineClient>,
    /// 30 Hz meter fan-out shared by all bridge connections (#148).
    pub meter_hub: Arc<MeterHub>,
}

impl AppState {
    /// Builds state with the production login limiter and HTTP service clients
    /// derived from `config`.
    pub fn new(config: Config, identity: Arc<dyn IdentityApi>) -> Self {
        let engine = default_engine(&config);
        let meter_hub = Arc::new(MeterHub::new());
        Self {
            music: default_music(&config),
            upload_jobs: default_jobs(&config),
            waveforms: default_waveforms(&config),
            tracks: default_tracks(&config),
            queues: default_queues(&config),
            control: default_control(&config),
            schedule: default_schedule(&config),
            shows: default_shows(&config),
            broadcasts: default_broadcasts(&config),
            announcements: default_announcements(&config),
            config: Arc::new(config),
            identity,
            limiter: Arc::new(WindowLimiter::new(LOGIN_MAX_ATTEMPTS, LOGIN_WINDOW)),
            engine,
            meter_hub,
        }
    }

    /// Overrides the engine client (tests inject a [`FakeEngineClient`]).
    pub fn with_engine(mut self, engine: Arc<dyn EngineClient>) -> Self {
        self.engine = engine;
        self
    }

    /// Overrides the meter hub (tests use a small broadcast capacity).
    pub fn with_meter_hub(mut self, meter_hub: Arc<MeterHub>) -> Self {
        self.meter_hub = meter_hub;
        self
    }

    /// Builds state with a custom limiter (tests).
    pub fn with_limiter(
        config: Config,
        identity: Arc<dyn IdentityApi>,
        limiter: Arc<WindowLimiter>,
    ) -> Self {
        let engine = default_engine(&config);
        let meter_hub = Arc::new(MeterHub::new());
        Self {
            music: default_music(&config),
            upload_jobs: default_jobs(&config),
            waveforms: default_waveforms(&config),
            tracks: default_tracks(&config),
            queues: default_queues(&config),
            control: default_control(&config),
            schedule: default_schedule(&config),
            shows: default_shows(&config),
            broadcasts: default_broadcasts(&config),
            announcements: default_announcements(&config),
            config: Arc::new(config),
            identity,
            limiter,
            engine,
            meter_hub,
        }
    }

    /// Builds state with explicit clients (upload + library tests).
    #[allow(clippy::too_many_arguments)]
    pub fn with_clients(
        config: Config,
        identity: Arc<dyn IdentityApi>,
        limiter: Arc<WindowLimiter>,
        music: Arc<dyn MusicApi>,
        upload_jobs: Arc<dyn UploadJobApi>,
        waveforms: Arc<dyn WaveformApi>,
        tracks: Arc<dyn TrackApi>,
        queues: Arc<dyn QueueApi>,
        control: Arc<dyn ControlApi>,
    ) -> Self {
        let schedule = default_schedule(&config);
        let shows = default_shows(&config);
        let broadcasts = default_broadcasts(&config);
        let announcements = default_announcements(&config);
        let engine = default_engine(&config);
        let meter_hub = Arc::new(MeterHub::new());
        Self {
            config: Arc::new(config),
            identity,
            music,
            upload_jobs,
            waveforms,
            tracks,
            queues,
            control,
            schedule,
            shows,
            broadcasts,
            announcements,
            limiter,
            engine,
            meter_hub,
        }
    }

    /// Builds state with explicit schedule/show/broadcast clients (schedule
    /// tests); the remaining clients are the production HTTP ones.
    #[allow(clippy::too_many_arguments)]
    pub fn with_schedule_clients(
        config: Config,
        identity: Arc<dyn IdentityApi>,
        limiter: Arc<WindowLimiter>,
        schedule: Arc<dyn ScheduleApi>,
        shows: Arc<dyn ShowApi>,
        broadcasts: Arc<dyn BroadcastApi>,
    ) -> Self {
        let engine = default_engine(&config);
        Self {
            music: default_music(&config),
            upload_jobs: default_jobs(&config),
            waveforms: default_waveforms(&config),
            tracks: default_tracks(&config),
            queues: default_queues(&config),
            control: default_control(&config),
            schedule,
            shows,
            broadcasts,
            announcements: default_announcements(&config),
            config: Arc::new(config),
            identity,
            limiter,
            engine,
            meter_hub: Arc::new(MeterHub::new()),
        }
    }
}

/// The production engine client derived from `config` (disabled when unset).
fn default_engine(config: &Config) -> Arc<dyn EngineClient> {
    Arc::new(LiquidsoapEngineClient::new(
        config.liquidsoap_control_url.clone(),
    ))
}

fn default_music(config: &Config) -> Arc<dyn MusicApi> {
    Arc::new(
        HttpMusicApi::new(config.music_api_url.clone(), config.music_api_token.clone())
            .expect("build music api client"),
    )
}

fn default_jobs(config: &Config) -> Arc<dyn UploadJobApi> {
    Arc::new(
        HttpUploadJobApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build upload-job client"),
    )
}

fn default_waveforms(config: &Config) -> Arc<dyn WaveformApi> {
    Arc::new(
        HttpWaveformApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build waveform client"),
    )
}

fn default_tracks(config: &Config) -> Arc<dyn TrackApi> {
    Arc::new(
        HttpTrackApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build track client"),
    )
}

fn default_queues(config: &Config) -> Arc<dyn QueueApi> {
    Arc::new(
        HttpQueueApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build queue client"),
    )
}

fn default_control(config: &Config) -> Arc<dyn ControlApi> {
    Arc::new(
        HttpControlApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build control client"),
    )
}

fn default_schedule(config: &Config) -> Arc<dyn ScheduleApi> {
    Arc::new(
        HttpScheduleApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build schedule client"),
    )
}

fn default_shows(config: &Config) -> Arc<dyn ShowApi> {
    Arc::new(
        HttpShowApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build show client"),
    )
}

fn default_broadcasts(config: &Config) -> Arc<dyn BroadcastApi> {
    Arc::new(
        HttpBroadcastApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build broadcast client"),
    )
}

fn default_announcements(config: &Config) -> Arc<dyn AnnouncementApi> {
    Arc::new(
        HttpAnnouncementApi::new(
            config.scheduler_base_url.clone(),
            config.service_token.clone(),
        )
        .expect("build announcement client"),
    )
}

/// Builds the full portal router.
pub fn build_router(state: AppState) -> Router {
    // One 30 Hz meter sampler per app state (#148); skipped while the engine
    // is disabled, so hermetic deployments never poll Liquidsoap.
    state.meter_hub.clone().start(state.engine.clone());

    let protected = Router::new()
        .route("/", get(handlers::dashboard))
        .route("/account", get(handlers::account_get))
        .route(
            "/account/password",
            axum::routing::post(handlers::account_password),
        )
        .route(
            "/account/stream-password",
            axum::routing::post(handlers::account_stream_password),
        )
        .route("/logout", axum::routing::post(handlers::logout))
        .route("/library", get(crate::library::library_get))
        .route("/library/tracks/{id}", get(crate::library::track_detail))
        .route(
            "/library/tracks/{id}/visibility",
            axum::routing::post(crate::library::track_visibility),
        )
        .route(
            "/library/tracks/{id}/delete",
            axum::routing::post(crate::library::track_delete),
        )
        // Lift axum's default 2 MiB Multipart body limit for this route only:
        // the streaming staging in `upload::stage_multipart` is the sole size
        // gate (`UPLOAD_MAX_BYTES` → 413 + temp cleanup), so large uploads
        // never get buffered or rejected at the framework layer (Issue #18/#58).
        .route(
            "/library/upload",
            axum::routing::post(upload::upload_post).layer(DefaultBodyLimit::disable()),
        )
        .route("/library/upload/{id}", get(upload::upload_fragment))
        .route(
            "/library/tracks/{id}/waveform.json",
            get(upload::waveform_json),
        )
        .route(
            "/library/tracks/{id}/waveform.svg",
            get(upload::waveform_svg),
        )
        .route(
            "/queues",
            get(crate::queues::queues_get).post(crate::queues::queues_post),
        )
        .route("/queues/{id}", get(crate::queues::queue_editor))
        // Issue #155: htmx source-catalog fragment (tracks) for the queue screen.
        .route("/queues/{id}/sources", get(crate::queues::queue_sources))
        .route(
            "/queues/{id}/items",
            axum::routing::post(crate::queues::item_add),
        )
        .route(
            "/queues/{id}/items/{item}/move",
            axum::routing::post(crate::queues::item_move),
        )
        .route(
            "/queues/{id}/items/{item}/delete",
            axum::routing::post(crate::queues::item_delete),
        )
        .route(
            "/queues/{id}/rename",
            axum::routing::post(crate::queues::queue_rename),
        )
        .route(
            "/queues/{id}/skip",
            axum::routing::post(crate::queues::queue_skip),
        )
        .route(
            "/queues/{id}/delete",
            axum::routing::post(crate::queues::queue_delete),
        )
        // Issue #22: Control Panel (SSE + polling fallback + live badge).
        .route("/control", get(crate::control::control_get))
        // Issue #149: minimal debug mixer UI over the WS bridge (control.read).
        .route(
            "/control/debug",
            get(crate::control_debug::control_debug_get),
        )
        .route("/control/state", get(crate::control::control_state))
        .route("/control/events", get(crate::control::control_events))
        .route("/control/badge", get(crate::control::control_badge))
        .route(
            "/control/source",
            axum::routing::post(crate::control::control_source),
        )
        .route(
            "/control/pause",
            axum::routing::post(crate::control::control_pause),
        )
        .route(
            "/control/skip",
            axum::routing::post(crate::control::control_skip),
        )
        .route(
            "/shows",
            get(crate::shows::shows_get).post(crate::shows::show_create),
        )
        .route(
            "/shows/{id}",
            get(crate::shows::show_detail).post(crate::shows::show_update),
        )
        .route(
            "/schedule",
            get(crate::schedule::schedule_get).post(crate::schedule::broadcast_create),
        )
        .route("/schedule/new", get(crate::schedule::broadcast_new))
        .route(
            "/schedule/{id}/edit",
            get(crate::schedule::broadcast_edit_get),
        )
        .route(
            "/schedule/{id}",
            axum::routing::post(crate::schedule::broadcast_update),
        )
        .route(
            "/schedule/{id}/delete",
            axum::routing::post(crate::schedule::broadcast_delete),
        )
        // Issue #119: broadcast lifecycle (RBAC + CSRF via the layers).
        .route(
            "/schedule/{id}/start",
            axum::routing::post(crate::schedule::broadcast_start),
        )
        .route(
            "/schedule/{id}/end",
            axum::routing::post(crate::schedule::broadcast_end),
        )
        .route(
            "/schedule/{id}/cancel",
            axum::routing::post(crate::schedule::broadcast_cancel),
        )
        // Issue #38: manual announcements (RBAC + audit via the scheduler).
        .route(
            "/announcements",
            get(crate::announcements::announcements_get),
        )
        .route(
            "/announcements/post",
            axum::routing::post(crate::announcements::announcement_post),
        )
        // Issue #101: admin user management (RBAC `caster.manage`).
        .route(
            "/admin/users",
            get(crate::admin::admin_users_get).post(crate::admin::admin_user_create),
        )
        .route(
            "/admin/users/{id}",
            axum::routing::post(crate::admin::admin_user_update),
        )
        .route(
            "/admin/users/{id}/stream-password",
            axum::routing::post(crate::admin::admin_user_stream_password),
        )
        // CSRF first, then session resolution (auth runs outermost).
        .route_layer(from_fn_with_state(state.clone(), middleware::require_csrf))
        .route_layer(from_fn_with_state(
            state.clone(),
            middleware::resolve_session,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/static/htmx.min.js", get(htmx_js))
        .route("/static/htmx-ext-sse.js", get(htmx_sse_js))
        .route("/static/app.css", get(app_css))
        .route("/static/queue.css", get(queue_css))
        .route("/static/sortable.min.js", get(sortable_js))
        .route("/static/waveform.js", get(waveform_js))
        .route("/static/fonts/VT323.woff2", get(vt323_woff2))
        .route(
            "/static/fonts/SpaceMono-Regular.woff2",
            get(space_mono_regular_woff2),
        )
        .route(
            "/static/fonts/SpaceMono-Bold.woff2",
            get(space_mono_bold_woff2),
        )
        // Issue #148: cockpit WebSocket bridge. Deliberately **outside** the
        // `protected` layer: a WS upgrade must not be answered with a `303
        // /login` redirect — auth happens in-band (Close `4401`).
        .route("/api/v1/control/ws", get(crate::control_ws::ws_handler))
        .route(
            "/login",
            get(handlers::login_get).post(handlers::login_post),
        )
        .merge(protected)
        .fallback(not_found)
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn htmx_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        HTMX_JS,
    )
}

async fn htmx_sse_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        HTMX_SSE_JS,
    )
}

async fn app_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn queue_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        QUEUE_CSS,
    )
}

async fn sortable_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        SORTABLE_JS,
    )
}

async fn waveform_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        WAVEFORM_JS,
    )
}

async fn vt323_woff2() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], VT323_WOFF2)
}

async fn space_mono_regular_woff2() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "font/woff2")],
        SPACE_MONO_REGULAR_WOFF2,
    )
}

async fn space_mono_bold_woff2() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "font/woff2")],
        SPACE_MONO_BOLD_WOFF2,
    )
}

async fn not_found() -> PortalError {
    PortalError::NotFound
}
