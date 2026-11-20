//! Public API: router, shared state, CORS/405 handling and `healthz`.

pub mod casters;
pub mod media;
pub mod shell;
pub mod tapes;
pub mod waveform;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sqlx::SqlitePool;

use crate::error::{ApiError, unavailable_body};
use media::MediaProxy;

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    /// Read-only pool over `zukka.db`.
    pub pool: SqlitePool,
    /// Absolute base URL for `*_url` fields (no trailing slash).
    pub base_url: String,
    /// Music API proxy (client + credentials) for `/media/{file}`.
    pub media: MediaProxy,
}

/// Builds the full application router: `/healthz` + `/api/v1` + `/media`.
pub fn router(
    pool: SqlitePool,
    base_url: String,
    music_api_url: String,
    music_api_token: String,
) -> Router {
    let state = AppState {
        pool,
        base_url,
        media: MediaProxy::new(music_api_url, music_api_token),
    };

    // Inner router: the API keeps its own JSON fallback, so an unknown
    // `/api/v1/*` path never renders HTML (plan E4/R8).
    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/tapes", get(tapes::get_tapes).options(preflight))
        .route("/tapes/{slug}", get(tapes::get_tape).options(preflight))
        .route(
            "/tapes/{slug}/waveform",
            get(waveform::get_waveform).options(preflight),
        )
        .route("/casters", get(casters::get_casters).options(preflight))
        .fallback(not_found);

    Router::new()
        .route("/healthz", get(root_health))
        .nest("/api/v1", api)
        .route("/", get(shell::get_index))
        .route("/t/{slug}", get(shell::get_tape))
        .route(
            "/media/{file}",
            get(media::get_media)
                .head(media::head_media)
                .options(preflight),
        )
        .route("/assets/{*path}", get(crate::assets::get_asset))
        .fallback(shell::html_not_found)
        .layer(middleware::from_fn(cors))
        .with_state(state)
}

/// `GET /healthz` — plain liveness, no DB access.
async fn root_health() -> &'static str {
    "ok"
}

/// `GET /api/v1/healthz` — liveness + a trivial `SELECT` against `tapes`.
///
/// A DB failure yields `503` with the §4.6 envelope; both responses are
/// `Cache-Control: no-store`.
pub async fn healthz(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1 FROM tapes LIMIT 1")
        .fetch_optional(&state.pool)
        .await
    {
        Ok(_) => {
            let mut res = Json(json!({ "status": "ok" })).into_response();
            res.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        Err(err) => {
            tracing::error!("healthz database check failed: {err:#}");
            let mut res =
                (StatusCode::SERVICE_UNAVAILABLE, Json(unavailable_body())).into_response();
            res.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
    }
}

/// Router fallback: unknown routes answer with the §4.6 envelope, not Axum's
/// default plain-text `404`.
async fn not_found() -> ApiError {
    ApiError::RouteNotFound
}

/// `OPTIONS` preflight for public routes → `204` with CORS headers.
async fn preflight() -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    let headers = res.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    res
}

/// Adds `Access-Control-Allow-Origin: *` to every response.
async fn cors(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}
