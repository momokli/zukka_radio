//! Announcement-timing handlers (Issue #36; thin wrappers over the store).
//!
//! Global timings live in `settings`, per-show overrides in
//! `show_announcement_timings`. No RBAC yet (TODO below), matching
//! `api/announcement_templates.rs`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{
    AnnouncementTimingsDto, ShowAnnouncementTimingsDto, UpdateAnnouncementTimingsRequest,
    UpdateShowAnnouncementTimingsRequest,
};
use crate::error::ApiResult;
use crate::store::announcement_timings;

// TODO(auth): handlers run without RBAC until the auth issue lands
// (permission `announcement.write`).

/// `GET /announcement-timings` — effective global timings (with defaults).
pub async fn get_global(State(pool): State<SqlitePool>) -> ApiResult<Json<AnnouncementTimingsDto>> {
    let timings = announcement_timings::global(&pool).await?;
    Ok(Json(announcement_timings::dto(timings)))
}

/// `PATCH /announcement-timings` — partial update of the global timings.
pub async fn patch_global(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<UpdateAnnouncementTimingsRequest>,
) -> ApiResult<Json<AnnouncementTimingsDto>> {
    let current = announcement_timings::global(&pool).await?;
    let next = announcement_timings::apply_patch(current, &req)?;
    let saved = announcement_timings::set_global(&pool, next).await?;
    Ok(Json(announcement_timings::dto(saved)))
}

/// `GET /announcement-timings/shows/{show_id}`
pub async fn get_show(
    State(pool): State<SqlitePool>,
    Path(show_id): Path<String>,
) -> ApiResult<Json<ShowAnnouncementTimingsDto>> {
    Ok(Json(announcement_timings::get(&pool, &show_id).await?))
}

/// `PUT /announcement-timings/shows/{show_id}` — upsert the overrides.
pub async fn put_show(
    State(pool): State<SqlitePool>,
    Path(show_id): Path<String>,
    ApiJson(req): ApiJson<UpdateShowAnnouncementTimingsRequest>,
) -> ApiResult<Json<ShowAnnouncementTimingsDto>> {
    Ok(Json(
        announcement_timings::upsert(&pool, &show_id, &req).await?,
    ))
}

/// `DELETE /announcement-timings/shows/{show_id}` — idempotent; `204`.
pub async fn delete_show(
    State(pool): State<SqlitePool>,
    Path(show_id): Path<String>,
) -> ApiResult<StatusCode> {
    announcement_timings::delete(&pool, &show_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
