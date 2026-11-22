//! Control endpoints (Issue #15) — portal/operator facing.
//!
//! Four thin handlers over [`crate::store::control`]. The routes stay open for
//! now; each handler carries a `TODO(auth)` marker (end state: portal service
//! token §4.1 + permission `control.write`, §6.10.1) — the RBAC middleware is a
//! separate issue, exactly like #10–#14.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{
    ControlCommandRequest, ControlCommandResponse, ControlLiveRequest, ControlPauseRequest,
    ControlPlanPutRequest, ControlPlanPutResponse, ControlSkipRequest, ControlStateDto,
    ControlSwitchRequest, PlanDto,
};
use crate::error::ApiResult;
use crate::store;

/// Builds the `/control` sub-router.
pub fn router() -> Router<SqlitePool> {
    // TODO(auth): portal service token + `control.write` permission (§4.1/§6.10.1).
    Router::new()
        .route("/state", get(state))
        .route("/plan", get(get_plan).put(put_plan))
        .route("/command", post(command))
        .route("/live", post(live))
        .route("/switch", post(switch))
        .route("/skip", post(skip))
        .route("/pause", post(pause))
}

/// `GET /control/state` — full read-only control-panel state (Issue #22).
pub async fn state(State(pool): State<SqlitePool>) -> ApiResult<axum::Json<ControlStateDto>> {
    // TODO(auth): require permission `control.read`.
    Ok(axum::Json(store::control::state(&pool).await?))
}

/// `GET /control/plan` — the persisted control plan (Issue #147, US-3).
pub async fn get_plan(State(pool): State<SqlitePool>) -> ApiResult<axum::Json<PlanDto>> {
    // TODO(auth): require permission `control.read`.
    Ok(axum::Json(store::control::get_plan(&pool).await?))
}

/// `PUT /control/plan` — merge the control plan + best-effort engine push.
pub async fn put_plan(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ControlPlanPutRequest>,
) -> ApiResult<axum::Json<ControlPlanPutResponse>> {
    // TODO(auth): require permission `control.write`.
    let (plan, engine) = store::control::put_plan(&pool, &req).await?;
    Ok(axum::Json(ControlPlanPutResponse {
        plan,
        engine: Some(engine),
    }))
}

/// `POST /control/command` — the engine command proxy (Issue #147, US-5).
pub async fn command(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ControlCommandRequest>,
) -> ApiResult<axum::Json<ControlCommandResponse>> {
    // TODO(auth): require permission `control.write`.
    Ok(axum::Json(store::control::command(&pool, &req).await?))
}

/// `POST /control/live` — set/clear the live override.
pub async fn live(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ControlLiveRequest>,
) -> ApiResult<StatusCode> {
    // TODO(auth): require permission `control.write`.
    store::control::live(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /control/switch` — force one of the six layers.
pub async fn switch(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ControlSwitchRequest>,
) -> ApiResult<StatusCode> {
    // TODO(auth): require permission `control.write`.
    store::control::switch(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /control/skip` — skip the currently playing item.
pub async fn skip(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ControlSkipRequest>,
) -> ApiResult<StatusCode> {
    // TODO(auth): require permission `control.write`.
    store::control::skip(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /control/pause` — pause/resume; live active ⇒ `409 live_active`.
pub async fn pause(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<ControlPauseRequest>,
) -> ApiResult<StatusCode> {
    // TODO(auth): require permission `control.write`.
    store::control::pause(&pool, &req).await?;
    Ok(StatusCode::NO_CONTENT)
}
