//! Read-only access to the append-only `event_log` (Issue #35).
//!
//! Writing stays with the domain stores (`broadcasts::insert_event_log`,
//! `control::log_control_event`, `announcements::mark_*`); this module only
//! serves `GET /event-log`.

use sqlx::SqlitePool;

use crate::dto::{EventLogEntryDto, EventLogEvent, EventLogParams};
use crate::error::{ApiError, ApiResult};
use crate::time::{fmt_api_time, parse_storage_time};

/// Row shape of `event_log`.
#[derive(Debug, sqlx::FromRow)]
struct EventLogRow {
    id: String,
    occurred_at: String,
    event: EventLogEvent,
    actor_caster_id: Option<String>,
    broadcast_id: Option<String>,
    caster_id: Option<String>,
    source_layer: Option<String>,
    details: String,
    created_at: String,
}

impl EventLogRow {
    fn into_dto(self) -> ApiResult<EventLogEntryDto> {
        Ok(EventLogEntryDto {
            id: self.id,
            occurred_at: fmt_api_time(parse_storage_time(&self.occurred_at)?),
            event: self.event,
            actor_caster_id: self.actor_caster_id,
            broadcast_id: self.broadcast_id,
            caster_id: self.caster_id,
            source_layer: self.source_layer,
            details: serde_json::from_str(&self.details).map_err(|err| {
                ApiError::Internal(anyhow::anyhow!("invalid event_log details: {err}"))
            })?,
            created_at: self.created_at,
        })
    }
}

/// Lists log entries (newest first, capped at 500).
///
/// `from` is inclusive, `to` exclusive, both on `occurred_at`; the remaining
/// filters are exact matches.
pub async fn list(pool: &SqlitePool, params: &EventLogParams) -> ApiResult<Vec<EventLogEntryDto>> {
    let event = match params.event.as_deref() {
        Some(raw) => Some(EventLogEvent::parse("event", raw)?),
        None => None,
    };
    let event = event.map(EventLogEvent::as_str);

    let rows: Vec<EventLogRow> = sqlx::query_as(
        "SELECT id, occurred_at, event, actor_caster_id, broadcast_id, caster_id, source_layer, \
         details, created_at FROM event_log \
         WHERE (? IS NULL OR occurred_at >= ?) AND (? IS NULL OR occurred_at < ?) \
         AND (? IS NULL OR event = ?) AND (? IS NULL OR broadcast_id = ?) \
         AND (? IS NULL OR caster_id = ?) \
         ORDER BY occurred_at DESC, id DESC LIMIT 500",
    )
    .bind(&params.from)
    .bind(&params.from)
    .bind(&params.to)
    .bind(&params.to)
    .bind(event)
    .bind(event)
    .bind(&params.broadcast_id)
    .bind(&params.broadcast_id)
    .bind(&params.caster_id)
    .bind(&params.caster_id)
    .fetch_all(pool)
    .await?;

    rows.into_iter().map(EventLogRow::into_dto).collect()
}
