//! Source handlers (thin wrappers over the store layer).
//!
//! C77: the layer set is fixed — only `GET` and `PATCH` are registered.

use axum::Json;
use axum::extract::{Path, State};
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{SourceDto, UpdateSourceRequest};
use crate::error::ApiResult;
use crate::store;

// TODO(auth): all handlers below run without RBAC until the auth issue lands.
// Permission markers: GET → sources.read, PATCH → sources.manage.

/// `GET /sources`
pub async fn list(State(pool): State<SqlitePool>) -> ApiResult<Json<Vec<SourceDto>>> {
    Ok(Json(store::sources::list(&pool).await?))
}

/// `GET /sources/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<SourceDto>> {
    Ok(Json(store::sources::get(&pool, &id).await?))
}

/// `PATCH /sources/{id}` — only `enabled` and/or `config` (C78).
pub async fn update(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateSourceRequest>,
) -> ApiResult<Json<SourceDto>> {
    Ok(Json(store::sources::update(&pool, &id, &req).await?))
}
