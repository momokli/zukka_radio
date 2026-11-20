//! Announcement handlers (Issue #35; thin wrappers over the store layer).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Json, response::IntoResponse};
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::api::rbac::require_permission;
use crate::dto::{
    AnnouncementDto, AnnouncementEvent, AnnouncementListParams, AnnouncementStatus,
    CreateAnnouncementRequest, UpdateAnnouncementRequest,
};
use crate::error::ApiResult;
use crate::store;
use crate::time::{fmt_storage_time, parse_api_time};

// Issue #38: the write routes guard on `announcement.write` (§6.10.1); the
// read routes stay open (the matrix knows only `announcement.write`).

/// `GET /announcements?status=&from=&to=&event=`
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(params): Query<AnnouncementListParams>,
) -> ApiResult<Json<Vec<AnnouncementDto>>> {
    let status = match params.status.as_deref() {
        Some(raw) => Some(AnnouncementStatus::parse("status", raw)?),
        None => None,
    };
    let event = match params.event.as_deref() {
        Some(raw) => Some(AnnouncementEvent::parse("event", raw)?),
        None => None,
    };
    let from = window_bound("from", params.from.as_deref())?;
    let to = window_bound("to", params.to.as_deref())?;
    let rows = store::announcements::list(&pool, status, event, from, to).await?;
    Ok(Json(rows))
}

/// `POST /announcements`
pub async fn create(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<CreateAnnouncementRequest>,
) -> ApiResult<impl IntoResponse> {
    let viewer = require_permission(&pool, &headers, "announcement.write").await?;
    let announcement = store::announcements::create(&pool, &req, Some(&viewer.caster_id)).await?;
    Ok((StatusCode::CREATED, Json(announcement)))
}

/// `GET /announcements/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<AnnouncementDto>> {
    Ok(Json(store::announcements::get(&pool, &id).await?))
}

/// `PATCH /announcements/{id}`
pub async fn update(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateAnnouncementRequest>,
) -> ApiResult<Json<AnnouncementDto>> {
    require_permission(&pool, &headers, "announcement.write").await?;
    Ok(Json(store::announcements::update(&pool, &id, &req).await?))
}

/// `DELETE /announcements/{id}`
pub async fn delete(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    require_permission(&pool, &headers, "announcement.write").await?;
    store::announcements::delete(&pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /announcements/{id}/send` — re-arm the queue only (no delivery).
pub async fn send(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<AnnouncementDto>> {
    let viewer = require_permission(&pool, &headers, "announcement.write").await?;
    Ok(Json(
        store::announcements::send(&pool, &id, &viewer.caster_id).await?,
    ))
}

/// Parses an optional `scheduled_at` window bound into storage form.
fn window_bound(field: &'static str, raw: Option<&str>) -> ApiResult<Option<String>> {
    match raw {
        Some(raw) => Ok(Some(fmt_storage_time(parse_api_time(field, raw)?))),
        None => Ok(None),
    }
}
