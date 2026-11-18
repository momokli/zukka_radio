//! Broadcast handlers (thin wrappers over the store layer).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{BroadcastDto, BroadcastStatus, CreateBroadcastRequest, UpdateBroadcastRequest};
use crate::error::ApiResult;
use crate::store;
use crate::time::parse_api_time;

/// Query parameters of `GET /broadcasts` (all optional).
#[derive(Debug, Deserialize)]
pub struct ListBroadcastsParams {
    pub from: Option<String>,
    pub to: Option<String>,
    pub caster_id: Option<String>,
    pub status: Option<String>,
}

// TODO(auth): handlers run without RBAC until the auth issue lands
// (permission `broadcast.write`).

/// `GET /broadcasts?from=&to=&caster_id=&status=`
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(params): Query<ListBroadcastsParams>,
) -> ApiResult<Json<Vec<BroadcastDto>>> {
    let from = params
        .from
        .as_deref()
        .map(|raw| parse_api_time("from", raw))
        .transpose()?;
    let to = params
        .to
        .as_deref()
        .map(|raw| parse_api_time("to", raw))
        .transpose()?;
    let status = params
        .status
        .as_deref()
        .map(|raw| BroadcastStatus::parse("status", raw))
        .transpose()?;

    let broadcasts =
        store::broadcasts::list(&pool, from, to, params.caster_id.as_deref(), status).await?;
    Ok(Json(broadcasts))
}

/// `POST /broadcasts`
pub async fn create(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateBroadcastRequest>,
) -> ApiResult<impl IntoResponse> {
    let broadcast = store::broadcasts::create(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(broadcast)))
}

/// `GET /broadcasts/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<BroadcastDto>> {
    Ok(Json(
        store::broadcasts::get_with_warnings(&pool, &id).await?,
    ))
}

/// `PATCH /broadcasts/{id}` (optimistic lock via `version`)
pub async fn update(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateBroadcastRequest>,
) -> ApiResult<Json<BroadcastDto>> {
    Ok(Json(store::broadcasts::update(&pool, &id, &req).await?))
}

/// `DELETE /broadcasts/{id}`
pub async fn delete(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    store::broadcasts::delete(&pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /broadcasts/{id}/cancel`
pub async fn cancel(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<BroadcastDto>> {
    Ok(Json(store::broadcasts::cancel(&pool, &id).await?))
}

/// `POST /broadcasts/{id}/start`
pub async fn start(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<BroadcastDto>> {
    Ok(Json(store::broadcasts::start(&pool, &id).await?))
}

/// `POST /broadcasts/{id}/end`
pub async fn end(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<BroadcastDto>> {
    Ok(Json(store::broadcasts::end(&pool, &id).await?))
}
