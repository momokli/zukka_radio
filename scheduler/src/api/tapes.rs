//! Tape handlers (thin wrappers over the store layer).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::api::viewer_caster_id;
use crate::dto::{ListTapesParams, TapeDto};
use crate::error::ApiResult;
use crate::store;

/// Query parameters of `GET /tapes` (all optional).
#[derive(Debug, Deserialize)]
pub struct ListTapesQuery {
    pub q: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub owner_caster_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

// TODO(auth): all handlers below run without RBAC until the auth issue lands.

/// `GET /tapes?q=&visibility=&owner_caster_id=&source=&status=&visible=&limit=&offset=`
pub async fn list(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Query(params): Query<ListTapesQuery>,
) -> ApiResult<Json<Vec<TapeDto>>> {
    let viewer = viewer_caster_id(&headers);
    let list_params = ListTapesParams {
        q: params.q,
        visibility: params.visibility,
        owner_caster_id: params.owner_caster_id,
        source: params.source,
        status: params.status,
        visible: params.visible,
        limit: params.limit,
        offset: params.offset,
    };
    let tapes = store::tapes::search(&pool, &list_params, viewer.as_deref()).await?;
    Ok(Json(tapes))
}

/// `GET /tapes/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<TapeDto>> {
    let viewer = viewer_caster_id(&headers);
    Ok(Json(
        store::tapes::get(&pool, &id, viewer.as_deref()).await?,
    ))
}
