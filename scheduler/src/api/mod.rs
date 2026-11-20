//! HTTP API: router builder + shared extractors.

pub mod queues;

pub mod announcement_templates;
pub mod announcements;
pub mod auth;
pub mod broadcasts;
pub mod control;
pub mod event_log;
pub mod playback;
pub mod schedule;
pub mod shows;
pub mod sources;
pub mod tape_jobs;
pub mod tracks;
pub mod upload_jobs;
pub mod waveforms;

use axum::extract::{FromRequest, Request};
use axum::http::HeaderMap;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use sqlx::SqlitePool;

use crate::error::ApiError;

/// A `Json` extractor that maps rejection into the §4.6 error envelope.
///
/// Malformed bodies become `400 bad_request`; unknown enum variants are
/// validated in the handlers so they surface as `422 validation_error` with a
/// precise `details.field`.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(ApiError::bad_request(format!(
                "invalid request body: {}",
                rejection.body_text()
            ))),
        }
    }
}

/// The canonical viewer-context header (§4.1). The portal sets it from the
/// session; it is never taken from client input.
const VIEWER_HEADER: &str = "x-caster-id";

/// Reads the viewer `caster_id` from `X-Caster-Id`; `None` when absent/blank.
pub fn viewer_caster_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(VIEWER_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Builds the full application router: `/healthz` + `/api/v1`.
pub fn router(pool: SqlitePool) -> Router {
    // TODO(auth): RBAC middleware (permission matrix §6.10.1) is a separate
    // issue; #10 ships the routes + handlers only.
    let api = Router::new()
        .route("/queues", get(queues::list).post(queues::create))
        .route(
            "/queues/{id}",
            get(queues::get_one)
                .patch(queues::update)
                .delete(queues::delete),
        )
        .route(
            "/queues/{id}/items",
            get(queues::list_items).post(queues::add_item),
        )
        .route(
            "/queues/{id}/items/{item_id}",
            axum::routing::patch(queues::update_item).delete(queues::delete_item),
        )
        .route("/queues/{id}/reorder", post(queues::reorder))
        .route("/queues/{id}/clear", post(queues::clear));

    let api = api
        .route("/shows", get(shows::list).post(shows::create))
        .route(
            "/shows/{id}",
            get(shows::get_one)
                .patch(shows::update)
                .delete(shows::delete),
        )
        .route(
            "/broadcasts",
            get(broadcasts::list).post(broadcasts::create),
        )
        .route(
            "/broadcasts/{id}",
            get(broadcasts::get_one)
                .patch(broadcasts::update)
                .delete(broadcasts::delete),
        )
        .route("/broadcasts/{id}/cancel", post(broadcasts::cancel))
        .route("/broadcasts/{id}/start", post(broadcasts::start))
        .route("/broadcasts/{id}/end", post(broadcasts::end))
        .route("/schedule", get(schedule::window))
        .route("/schedule/expand", post(schedule::expand));

    // Issue #34: announcement templates. `{id}/render` is a longer static
    // suffix than `{id}`, so Axum matches it first.
    let api = api
        .route(
            "/announcement-templates",
            get(announcement_templates::list).post(announcement_templates::create),
        )
        .route(
            "/announcement-templates/{id}",
            get(announcement_templates::get_one)
                .patch(announcement_templates::update)
                .delete(announcement_templates::delete),
        )
        .route(
            "/announcement-templates/{id}/render",
            post(announcement_templates::render_preview),
        );

    // Issue #35: announcements (M5 — Events) + the read-only event log.
    // `{id}/send` is a longer static suffix than `{id}`, so Axum matches it
    // first.
    let api = api
        .route(
            "/announcements",
            get(announcements::list).post(announcements::create),
        )
        .route(
            "/announcements/{id}",
            get(announcements::get_one)
                .patch(announcements::update)
                .delete(announcements::delete),
        )
        .route("/announcements/{id}/send", post(announcements::send))
        .route("/event-log", get(event_log::list));

    // C77: the six layers are fixed — read + PATCH only, no POST/DELETE/PUT.
    let api = api.route("/sources", get(sources::list)).route(
        "/sources/{id}",
        get(sources::get_one).patch(sources::update),
    );

    // Issue #19: track library. Viewer context via `X-Caster-Id` (TODO(auth));
    // `/tracks/import` is a static segment, matched before `/tracks/{id}`.
    let api = api
        .route("/tracks", get(tracks::list).post(tracks::create))
        .route("/tracks/import", post(tracks::import))
        .route(
            "/tracks/{id}",
            get(tracks::get_one)
                .patch(tracks::update)
                .delete(tracks::delete),
        );

    // Issue #13: the token layer lives inside the playback sub-router, so it
    // guards only `/playback/*`; every route above stays open (TODO(auth)).
    let api = api.nest("/playback", playback::router());

    // Issue #15: operator control endpoints — open for now (TODO(auth)).
    let api = api.nest("/control", control::router());

    // Issue #16: identity authority. Every `/internal/*` route requires the
    // portal service token (§4.1); RBAC stays a portal concern (§6.10.1).
    let internal = Router::new()
        .route("/auth/verify", post(auth::verify))
        .route("/harbor/auth", post(auth::harbor_auth))
        .route("/sessions", post(auth::create_session))
        .route("/sessions/current", get(auth::current_session))
        .route(
            "/sessions/{id}",
            patch(auth::touch_session).delete(auth::revoke_session),
        )
        .route("/casters/{id}", get(auth::get_caster))
        .route("/casters/{id}/password", post(auth::change_password))
        .route(
            "/casters/{id}/stream-password",
            post(auth::change_stream_password),
        )
        .route(
            "/upload-jobs",
            get(upload_jobs::list).post(upload_jobs::create),
        )
        .route(
            "/upload-jobs/{id}",
            get(upload_jobs::get_one).patch(upload_jobs::patch),
        )
        .route("/tape-jobs/claim", post(tape_jobs::claim))
        .route("/tape-jobs/{id}", patch(tape_jobs::patch))
        .route("/waveforms", post(waveforms::upsert))
        .route(
            "/waveforms/{subject_kind}/{subject_id}",
            get(waveforms::get_one),
        )
        .route_layer(axum::middleware::from_fn(auth::require_service_token));
    let api = api.nest("/internal", internal);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", api)
        .fallback(not_found)
        .with_state(pool)
}

/// Router fallback: unknown routes answer with the error envelope, not Axum's
/// default plain-text `404`.
async fn not_found() -> ApiError {
    ApiError::RouteNotFound
}
