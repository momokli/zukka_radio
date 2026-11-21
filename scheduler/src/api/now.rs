//! `GET /api/v1/now` — internal public-safe now-playing (§4.2, Issue #107).
//!
//! The only route outside `/internal/*` that carries the service-token layer:
//! it is consumed exclusively by the tapes service (`SCHEDULER_SERVICE_TOKEN`)
//! to build the public `/api/v1/public/nowplaying` projection.

use axum::extract::State;
use axum::middleware;
use axum::routing::get;
use axum::{Json, Router};
use sqlx::SqlitePool;

use crate::api::auth;
use crate::dto::NowPlayingDto;
use crate::error::ApiResult;

/// Builds the `/now` sub-router with the service-token layer applied.
///
/// `route_layer` guards **only** these routes; the remaining `/api/v1` routes
/// stay open (the existing `TODO(auth)` state).
pub fn router() -> Router<SqlitePool> {
    Router::new()
        .route("/", get(now))
        .route_layer(middleware::from_fn(auth::require_service_token))
}

/// `GET /api/v1/now` — the public-safe now-playing snapshot.
pub async fn now(State(pool): State<SqlitePool>) -> ApiResult<Json<NowPlayingDto>> {
    Ok(Json(crate::store::now::now_playing(&pool).await?))
}
