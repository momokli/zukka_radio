//! Show handlers (thin wrappers over the store layer).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{CreateShowRequest, ShowDto, UpdateShowRequest};
use crate::error::ApiResult;
use crate::store;

/// Query parameters of `GET /shows` (all optional).
#[derive(Debug, Deserialize)]
pub struct ListShowsParams {
    pub caster_id: Option<String>,
}

// TODO(auth): handlers run without RBAC until the auth issue lands
// (permission `show.write`).

/// `GET /shows?caster_id=`
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(params): Query<ListShowsParams>,
) -> ApiResult<Json<Vec<ShowDto>>> {
    Ok(Json(
        store::shows::list(&pool, params.caster_id.as_deref()).await?,
    ))
}

/// `POST /shows`
pub async fn create(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateShowRequest>,
) -> ApiResult<impl IntoResponse> {
    let show = store::shows::create(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(show)))
}

/// `GET /shows/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<ShowDto>> {
    Ok(Json(store::shows::get(&pool, &id).await?))
}

/// `PATCH /shows/{id}`
pub async fn update(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateShowRequest>,
) -> ApiResult<Json<ShowDto>> {
    Ok(Json(store::shows::update(&pool, &id, &req).await?))
}

/// `DELETE /shows/{id}`
pub async fn delete(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    store::shows::delete(&pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
