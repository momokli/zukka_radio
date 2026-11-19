//! Upload-job write-through endpoints (Issue #18, §4.2).
//!
//! Only the scheduler writes `upload_jobs` (P2); the portal creates and
//! patches jobs through these `/internal/*` routes, guarded by the service
//! token middleware in [`crate::api::auth::require_service_token`].

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{CreateUploadJobRequest, PatchUploadJobRequest, UploadJobDto};
use crate::error::{ApiError, ApiResult};
use crate::store;

/// `?limit=` for `GET /internal/upload-jobs`.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Response of `POST /internal/upload-jobs` — `{id}`.
#[derive(Debug, Serialize)]
pub struct CreateUploadJobResponse {
    pub id: String,
}

/// `POST /internal/upload-jobs` — create a `pending` job → `201 {id}`.
pub async fn create(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateUploadJobRequest>,
) -> ApiResult<(StatusCode, Json<CreateUploadJobResponse>)> {
    if req.caster_id.trim().is_empty() {
        return Err(ApiError::validation(
            "caster_id",
            "caster_id must not be empty",
        ));
    }
    if req.filename.trim().is_empty() {
        return Err(ApiError::validation(
            "filename",
            "filename must not be empty",
        ));
    }
    let job = store::upload_jobs::create(&pool, &req).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateUploadJobResponse { id: job.id }),
    ))
}

/// `GET /internal/upload-jobs` — list recent jobs (newest first).
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<UploadJobDto>>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(store::upload_jobs::list(&pool, limit).await?))
}

/// `GET /internal/upload-jobs/{id}` — one job or `404`.
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<UploadJobDto>> {
    Ok(Json(store::upload_jobs::get(&pool, &id).await?))
}

/// `PATCH /internal/upload-jobs/{id}` — state transition → `200`.
pub async fn patch(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<PatchUploadJobRequest>,
) -> ApiResult<Json<UploadJobDto>> {
    if !store::upload_jobs::STATES.contains(&req.state.as_str()) {
        return Err(ApiError::validation(
            "state",
            format!(
                "unknown state '{}', expected one of 'pending', 'running', 'done', 'failed'",
                req.state
            ),
        ));
    }
    Ok(Json(
        store::upload_jobs::patch_state(&pool, &id, &req).await?,
    ))
}
