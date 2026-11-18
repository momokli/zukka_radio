//! Schedule handlers: merged window and idempotent expansion.

use axum::Json;
use axum::extract::{Query, State};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::dto::{ExpandResultDto, ScheduleEntryDto};
use crate::error::{ApiError, ApiResult};
use crate::store;
use crate::time::parse_api_time;

/// Query parameters of `GET /schedule` and `POST /schedule/expand`.
#[derive(Debug, Deserialize)]
pub struct WindowParams {
    pub from: Option<String>,
    pub to: Option<String>,
}

// TODO(auth): handlers run without RBAC until the auth issue lands
// (permission `show.write` / `broadcast.write`).

fn parse_window(params: &WindowParams) -> ApiResult<(DateTime<Utc>, DateTime<Utc>)> {
    let from_raw = params
        .from
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("missing query parameter 'from'"))?;
    let to_raw = params
        .to
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("missing query parameter 'to'"))?;
    let from = parse_api_time("from", from_raw)?;
    let to = parse_api_time("to", to_raw)?;
    if from > to {
        return Err(ApiError::validation(
            "from",
            "from must be before or equal to to",
        ));
    }
    Ok((from, to))
}

/// `GET /schedule?from=&to=`
pub async fn window(
    State(pool): State<SqlitePool>,
    Query(params): Query<WindowParams>,
) -> ApiResult<Json<Vec<ScheduleEntryDto>>> {
    let (from, to) = parse_window(&params)?;
    Ok(Json(store::schedule::merged_window(&pool, from, to).await?))
}

/// `POST /schedule/expand?from=&to=`
pub async fn expand(
    State(pool): State<SqlitePool>,
    Query(params): Query<WindowParams>,
) -> ApiResult<Json<ExpandResultDto>> {
    let (from, to) = parse_window(&params)?;
    Ok(Json(store::schedule::expand(&pool, from, to).await?))
}
