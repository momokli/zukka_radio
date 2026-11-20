//! Announcement-template handlers (thin wrappers over the store layer).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::announcement::render;
use crate::api::ApiJson;
use crate::dto::{
    AnnouncementTemplateDto, CreateAnnouncementTemplateRequest, RenderTemplateRequest,
    RenderTemplateResponse, UpdateAnnouncementTemplateRequest,
};
use crate::error::ApiResult;
use crate::store;

/// Query parameters of `GET /announcement-templates` (all optional).
#[derive(Debug, Deserialize)]
pub struct ListTemplatesParams {
    pub event: Option<String>,
    pub show_id: Option<String>,
}

// TODO(auth): handlers run without RBAC until the auth issue lands
// (permission `announcement.write`).

/// `GET /announcement-templates?event=&show_id=`
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(params): Query<ListTemplatesParams>,
) -> ApiResult<Json<Vec<AnnouncementTemplateDto>>> {
    let templates = store::announcement_templates::list(
        &pool,
        params.event.as_deref(),
        params.show_id.as_deref(),
    )
    .await?;
    Ok(Json(templates))
}

/// `POST /announcement-templates`
pub async fn create(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateAnnouncementTemplateRequest>,
) -> ApiResult<impl IntoResponse> {
    let template = store::announcement_templates::create(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(template)))
}

/// `GET /announcement-templates/{id}`
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<AnnouncementTemplateDto>> {
    Ok(Json(store::announcement_templates::get(&pool, &id).await?))
}

/// `PATCH /announcement-templates/{id}`
pub async fn update(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<UpdateAnnouncementTemplateRequest>,
) -> ApiResult<Json<AnnouncementTemplateDto>> {
    Ok(Json(
        store::announcement_templates::update(&pool, &id, &req).await?,
    ))
}

/// `DELETE /announcement-templates/{id}`
pub async fn delete(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    store::announcement_templates::delete(&pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /announcement-templates/{id}/render` — stateless preview.
pub async fn render_preview(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<RenderTemplateRequest>,
) -> ApiResult<Json<RenderTemplateResponse>> {
    let template = store::announcement_templates::get(&pool, &id).await?;
    let rendered = render::render_detailed(&template.body, &req.payload);
    Ok(Json(RenderTemplateResponse {
        rendered: rendered.text,
        unknown_placeholders: rendered.unknown,
    }))
}
