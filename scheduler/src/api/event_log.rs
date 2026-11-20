//! Event-log handler (Issue #35; read-only).

use axum::Json;
use axum::extract::{Query, State};
use sqlx::SqlitePool;

use crate::dto::{EventLogEntryDto, EventLogParams};
use crate::error::ApiResult;
use crate::store;
use crate::time::{fmt_storage_time, parse_api_time};

// TODO(auth): RBAC `event_log.read` lands with the auth issue (#10).

/// `GET /event-log?from=&to=&event=&broadcast_id=&caster_id=`
pub async fn list(
    State(pool): State<SqlitePool>,
    Query(params): Query<EventLogParams>,
) -> ApiResult<Json<Vec<EventLogEntryDto>>> {
    let from = window_bound("from", params.from.as_deref())?;
    let to = window_bound("to", params.to.as_deref())?;
    let params = EventLogParams {
        from,
        to,
        event: params.event,
        broadcast_id: params.broadcast_id,
        caster_id: params.caster_id,
    };
    let rows = store::event_log::list(&pool, &params).await?;
    Ok(Json(rows))
}

fn window_bound(field: &'static str, raw: Option<&str>) -> ApiResult<Option<String>> {
    match raw {
        Some(raw) => Ok(Some(fmt_storage_time(parse_api_time(field, raw)?))),
        None => Ok(None),
    }
}
