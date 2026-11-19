//! Track handlers (Issue #19 "Library") — thin wrappers over the store layer.
//!
//! The viewer context comes from the `X-Caster-Id` header (§4.1). It is set by
//! the portal from the session and is never trusted from the client; a missing
//! header (`None`) only exposes `shared` tracks.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Json, response::IntoResponse};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::api::{ApiJson, viewer_caster_id};
use crate::dto::{CreateTrackRequest, ImportTrackRequest, TrackDto, UpdateTrackRequest};
use crate::error::ApiResult;
use crate::store;

/// Query parameters of `GET /tracks` (all optional).
#[derive(Debug, Deserialize)]
pub struct ListTracksParams {
    pub q: Option<String>,
    pub visibility: Option<String>,
    pub owner_caster_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// `GET /tracks?q=&visibility=&owner_caster_id=&limit=&offset=`
pub async fn list(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Query(params): Query<ListTracksParams>,
) -> ApiResult<Json<Vec<TrackDto>>> {
    let viewer = viewer_caster_id(&headers);
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let tracks = store::tracks::search(
        &pool,
        params.q.as_deref(),
        params.visibility.as_deref(),
        params.owner_caster_id.as_deref(),
        viewer.as_deref(),
        limit,
        offset,
    )
    .await?;
    Ok(Json(tracks))
}

/// `POST /tracks`
pub async fn create(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateTrackRequest>,
) -> ApiResult<impl IntoResponse> {
    let track = store::tracks::create(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(track)))
}

/// `POST /tracks/import`
pub async fn import(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ImportTrackRequest>,
) -> ApiResult<impl IntoResponse> {
    let track = store::tracks::import(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(track)))
}

/// `GET /tracks/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<TrackDto>> {
    let viewer = viewer_caster_id(&headers);
    Ok(Json(
        store::tracks::get(&pool, &id, viewer.as_deref()).await?,
    ))
}

/// `PATCH /tracks/{id}`
pub async fn update(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateTrackRequest>,
) -> ApiResult<Json<TrackDto>> {
    let viewer = viewer_caster_id(&headers);
    Ok(Json(
        store::tracks::update(&pool, &id, &req, viewer.as_deref()).await?,
    ))
}

/// `DELETE /tracks/{id}`
pub async fn delete(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let viewer = viewer_caster_id(&headers);
    store::tracks::delete(&pool, &id, viewer.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}
