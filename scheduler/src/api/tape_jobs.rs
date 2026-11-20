//! Tape-job write-through endpoints (Issue #25, M3, §4.2/§3.8).
//!
//! The read-only tapes-worker is the only consumer: it claims due `probe` /
//! `peaks` jobs and reports progress. Both routes live inside the
//! service-token-guarded `/internal` router.

use axum::Json;
use axum::extract::{Path, State};
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{ClaimTapeJobsRequest, ClaimTapeJobsResponse, PatchTapeJobRequest, TapeJobDto};
use crate::error::{ApiError, ApiResult};
use crate::store;

const DEFAULT_LEASE_SECS: i64 = 600;

/// `POST /internal/tape-jobs/claim` — atomically claim due jobs → `200 {jobs}`.
pub async fn claim(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ClaimTapeJobsRequest>,
) -> ApiResult<Json<ClaimTapeJobsResponse>> {
    let kinds = match req.kinds {
        Some(kinds) => {
            if kinds.is_empty() {
                return Err(ApiError::validation(
                    "kinds",
                    "kinds must contain at least one of 'probe', 'peaks'",
                ));
            }
            for kind in &kinds {
                if !store::tape_jobs::KINDS.contains(&kind.as_str()) {
                    return Err(ApiError::validation(
                        "kinds",
                        format!("unknown kind '{kind}', expected 'probe' or 'peaks'"),
                    ));
                }
            }
            kinds
        }
        None => store::tape_jobs::KINDS
            .iter()
            .map(|kind| (*kind).to_string())
            .collect(),
    };

    let limit = req.limit.unwrap_or(1);
    if !(1..=16).contains(&limit) {
        return Err(ApiError::validation(
            "limit",
            "limit must be an integer in 1..=16",
        ));
    }

    let lease_secs = req.lease_secs.unwrap_or(DEFAULT_LEASE_SECS);
    if !(0..=86_400).contains(&lease_secs) {
        return Err(ApiError::validation(
            "lease_secs",
            "lease_secs must be an integer in 0..=86400",
        ));
    }

    let jobs = store::tape_jobs::claim(&pool, &kinds, limit, lease_secs).await?;
    Ok(Json(ClaimTapeJobsResponse { jobs }))
}

/// `PATCH /internal/tape-jobs/{id}` — state transition + probe metadata.
pub async fn patch(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<PatchTapeJobRequest>,
) -> ApiResult<Json<TapeJobDto>> {
    if !store::tape_jobs::STATES.contains(&req.state.as_str()) {
        return Err(ApiError::validation(
            "state",
            format!(
                "unknown state '{}', expected one of 'pending', 'running', 'done', 'failed'",
                req.state
            ),
        ));
    }
    Ok(Json(store::tape_jobs::patch(&pool, &id, &req).await?))
}
