//! Waveform write-through endpoints (Issue #18, D8/C05, §4.2).
//!
//! Only the scheduler writes `waveforms` (P2); the portal (and the tapes-worker)
//! upsert and read peaks through these `/internal/*` routes, guarded by the
//! service-token middleware in [`crate::api::auth::require_service_token`].

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{UpsertWaveformRequest, WaveformDto};
use crate::error::ApiResult;
use crate::store;

/// `POST /internal/waveforms` — upsert one waveform → `201` with the row.
pub async fn upsert(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<UpsertWaveformRequest>,
) -> ApiResult<(StatusCode, Json<WaveformDto>)> {
    let dto = store::waveforms::upsert(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(dto)))
}

/// `GET /internal/waveforms/{subject_kind}/{subject_id}` — one waveform or `404`.
pub async fn get_one(
    State(pool): State<SqlitePool>,
    Path((subject_kind, subject_id)): Path<(String, String)>,
) -> ApiResult<Json<WaveformDto>> {
    Ok(Json(
        store::waveforms::get(&pool, &subject_kind, &subject_id).await?,
    ))
}
