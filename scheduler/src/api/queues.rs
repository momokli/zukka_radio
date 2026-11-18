//! Queue + queue-item handlers (thin wrappers over the store layer).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{
    CreateItemRequest, CreateQueueRequest, QueueDto, QueueItemDto, QueueKind, ReorderRequest,
    UpdateItemRequest, UpdateQueueRequest,
};
use crate::error::ApiResult;
use crate::store;

/// Query parameters of `GET /queues` (both optional).
#[derive(Debug, Deserialize)]
pub struct ListQueuesParams {
    pub caster_id: Option<String>,
    pub kind: Option<String>,
}

// TODO(auth): all handlers below run without RBAC until the auth issue lands.

/// `GET /queues?caster_id=&kind=`
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(params): Query<ListQueuesParams>,
) -> ApiResult<Json<Vec<QueueDto>>> {
    let kind = match &params.kind {
        Some(raw) => Some(QueueKind::parse("kind", raw)?),
        None => None,
    };
    let queues = store::queues::list(&pool, params.caster_id.as_deref(), kind).await?;
    Ok(Json(queues))
}

/// `POST /queues`
pub async fn create(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateQueueRequest>,
) -> ApiResult<impl IntoResponse> {
    let queue = store::queues::create(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(queue)))
}

/// `GET /queues/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<QueueDto>> {
    Ok(Json(store::queues::get(&pool, &id).await?))
}

/// `PATCH /queues/{id}` (optimistic lock via `version`)
pub async fn update(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateQueueRequest>,
) -> ApiResult<Json<QueueDto>> {
    Ok(Json(store::queues::update(&pool, &id, &req).await?))
}

/// `DELETE /queues/{id}`
pub async fn delete(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    store::queues::delete(&pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /queues/{id}/items`
pub async fn list_items(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<QueueItemDto>>> {
    Ok(Json(store::queue_items::list(&pool, &id).await?))
}

/// `POST /queues/{id}/items`
pub async fn add_item(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<CreateItemRequest>,
) -> ApiResult<impl IntoResponse> {
    let item = store::queue_items::add(&pool, &id, &req).await?;
    Ok((StatusCode::CREATED, Json(item)))
}

/// `PATCH /queues/{id}/items/{item_id}`
pub async fn update_item(
    State(pool): State<SqlitePool>,
    Path((id, item_id)): Path<(String, String)>,
    ApiJson(req): ApiJson<UpdateItemRequest>,
) -> ApiResult<Json<QueueItemDto>> {
    Ok(Json(
        store::queue_items::update(&pool, &id, &item_id, &req).await?,
    ))
}

/// `DELETE /queues/{id}/items/{item_id}`
pub async fn delete_item(
    State(pool): State<SqlitePool>,
    Path((id, item_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    store::queue_items::delete(&pool, &id, &item_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /queues/{id}/reorder` (transactional, version-guarded)
pub async fn reorder(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<ReorderRequest>,
) -> ApiResult<Json<QueueDto>> {
    Ok(Json(store::queues::reorder(&pool, &id, &req).await?))
}

/// `POST /queues/{id}/clear`
pub async fn clear(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    store::queues::clear(&pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
