//! HTTP API: router builder + shared extractors.

pub mod queues;

pub mod broadcasts;
pub mod shows;

use axum::extract::{FromRequest, Request};
use axum::routing::{get, post};
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
        .route("/broadcasts/{id}/end", post(broadcasts::end));

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
