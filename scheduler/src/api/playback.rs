//! Playback endpoints (Issue #13) — Liquidsoap-facing.
//!
//! This sub-router is the **only** router carrying the token layer
//! ([`crate::api::auth::require_token`]); every other route stays open until
//! the RBAC issue lands (the `TODO(auth)` in [`crate::api`] remains).

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::api::auth;
use crate::dto::{
    PlaybackEndedRequest, PlaybackErrorRequest, PlaybackLayer, PlaybackStartedRequest,
};
use crate::error::ApiResult;
use crate::store;

/// Query parameters of `GET /playback/next`.
#[derive(Debug, Deserialize)]
pub struct NextParams {
    pub layer: String,
}

/// Builds the `/playback` sub-router with the token layer applied.
pub fn router() -> Router<SqlitePool> {
    Router::new()
        .route("/next", get(next))
        .route("/current", get(current))
        .route("/started", post(started))
        .route("/ended", post(ended))
        .route("/error", post(error))
        .route("/rotation.m3u", get(rotation_m3u))
        .layer(middleware::from_fn(auth::require_token))
}

/// `GET /playback/next?layer=show|queue`
pub async fn next(
    State(pool): State<SqlitePool>,
    Query(params): Query<NextParams>,
) -> ApiResult<Response> {
    let layer = PlaybackLayer::parse("layer", &params.layer)?;
    Ok(match store::playback::next(&pool, layer).await? {
        Some(item) => (StatusCode::OK, Json(item)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

/// `GET /playback/current`
pub async fn current(State(pool): State<SqlitePool>) -> ApiResult<Response> {
    Ok(match store::playback::current(&pool).await? {
        Some(item) => (StatusCode::OK, Json(item)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

/// `POST /playback/started`
pub async fn started(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<PlaybackStartedRequest>,
) -> ApiResult<StatusCode> {
    store::playback::started(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /playback/ended`
pub async fn ended(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<PlaybackEndedRequest>,
) -> ApiResult<StatusCode> {
    store::playback::ended(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /playback/rotation.m3u`
///
/// Renders the rotation, best-effort refreshes the on-disk file (`watch`
/// reload) and returns the rendered content with `audio/x-mpegurl`. A write
/// failure never turns the response into a `500` — the body is served anyway.
pub async fn rotation_m3u(State(pool): State<SqlitePool>) -> ApiResult<Response> {
    let body = store::rotation::render(&pool).await?;
    if let Err(err) = store::rotation::sync(&pool).await {
        tracing::warn!("failed to sync rotation.m3u: {err}");
    }
    Response::builder()
        .header(CONTENT_TYPE, "audio/x-mpegurl")
        .body(Body::from(body))
        .map_err(|err| crate::error::ApiError::Internal(anyhow::Error::new(err)))
}

/// `POST /playback/error`
pub async fn error(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<PlaybackErrorRequest>,
) -> ApiResult<StatusCode> {
    store::playback::error(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}
