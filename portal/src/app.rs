//! Application state + router.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::header;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::api_client::IdentityApi;
use crate::auth::{handlers, middleware};
use crate::config::Config;
use crate::error::PortalError;
use crate::rate_limit::WindowLimiter;

/// Login rate limit: 5 failures / 15 min (portal.md D3).
pub const LOGIN_MAX_ATTEMPTS: usize = 5;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

const HTMX_JS: &str = include_str!("../static/htmx.min.js");
const APP_CSS: &str = include_str!("../static/app.css");

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub identity: Arc<dyn IdentityApi>,
    pub limiter: Arc<WindowLimiter>,
}

impl AppState {
    /// Builds state with the production login limiter.
    pub fn new(config: Config, identity: Arc<dyn IdentityApi>) -> Self {
        Self {
            config: Arc::new(config),
            identity,
            limiter: Arc::new(WindowLimiter::new(LOGIN_MAX_ATTEMPTS, LOGIN_WINDOW)),
        }
    }

    /// Builds state with a custom limiter (tests).
    pub fn with_limiter(
        config: Config,
        identity: Arc<dyn IdentityApi>,
        limiter: Arc<WindowLimiter>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            identity,
            limiter,
        }
    }
}

/// Builds the full portal router.
pub fn build_router(state: AppState) -> Router {
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
        // CSRF first, then session resolution (auth runs outermost).
        .route_layer(from_fn_with_state(state.clone(), middleware::require_csrf))
        .route_layer(from_fn_with_state(
            state.clone(),
            middleware::resolve_session,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/static/htmx.min.js", get(htmx_js))
        .route("/static/app.css", get(app_css))
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

async fn app_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn not_found() -> PortalError {
    PortalError::NotFound
}
