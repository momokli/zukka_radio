//! `GET /api/v1/casters` — caster aggregate.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::error::ApiResult;
use crate::store;

const CASTERS_CACHE: &str = "public, max-age=60";

/// `GET /api/v1/casters` — casters with `tape_count > 0`, sorted by name.
pub async fn get_casters(State(state): State<AppState>) -> ApiResult<Response> {
    let casters = store::casters::list_public(&state.pool).await?;
    let mut res = (StatusCode::OK, Json(casters)).into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CASTERS_CACHE),
    );
    Ok(res)
}
