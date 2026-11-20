//! Announcement store (Issue #35): CRUD, dedupe-idempotent emit, the retry
//! state machine and the template resolution used by every emitter.
//!
//! Schema wins (`0001_init.sql`): TEXT PK (UUIDv7), `channel CHECK IN
//! ('telegram')`, `status CHECK IN (pending, sent, failed, cancelled)`,
//! `dedupe_key UNIQUE` (nullable — several `NULL` rows allowed).
//!
//! Consistency rule (§4): automatic emitters are **idempotent** (a collision
//! returns the existing row, never an error); an explicit `POST` with a
//! caller-supplied `dedupe_key` is a uniqueness claim and maps to
//! `409 duplicate_announcement`.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::announcement::{events, retry};
use crate::dto::{
    AnnouncementChannel, AnnouncementDto, AnnouncementEvent, AnnouncementStatus,
    CreateAnnouncementRequest, UpdateAnnouncementRequest,
};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
use crate::store::now_ts;
use crate::time::{fmt_api_time, fmt_storage_time, parse_api_time, parse_storage_time};
use crate::uuid::new_id;

/// Row shape of `announcements`, mapped onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct AnnouncementRow {
    id: String,
    event: AnnouncementEvent,
    show_id: Option<String>,
    broadcast_id: Option<String>,
    caster_id: Option<String>,
    template_id: Option<String>,
    channel: AnnouncementChannel,
    payload: String,
    dedupe_key: Option<String>,
    scheduled_at: Option<String>,
    sent_at: Option<String>,
    status: AnnouncementStatus,
    attempts: i64,
    next_attempt_at: Option<String>,
    error: Option<String>,
    created_at: String,
    updated_at: String,
}

impl AnnouncementRow {
    fn into_dto(self) -> ApiResult<AnnouncementDto> {
        Ok(AnnouncementDto {
            id: self.id,
            event: self.event,
            show_id: self.show_id,
            broadcast_id: self.broadcast_id,
            caster_id: self.caster_id,
            template_id: self.template_id,
            channel: self.channel,
            payload: parse_payload(&self.payload)?,
            dedupe_key: self.dedupe_key,
            scheduled_at: opt_api_time(self.scheduled_at)?,
            sent_at: opt_api_time(self.sent_at)?,
            status: self.status,
            attempts: self.attempts,
            next_attempt_at: opt_api_time(self.next_attempt_at)?,
            error: self.error,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "SELECT id, event, show_id, broadcast_id, caster_id, template_id, channel, \
     payload, dedupe_key, scheduled_at, sent_at, status, attempts, next_attempt_at, error, \
     created_at, updated_at FROM announcements";

const INSERT: &str = "INSERT INTO announcements \
     (id, event, show_id, broadcast_id, caster_id, template_id, channel, payload, dedupe_key, \
      scheduled_at, status, attempts, created_at, updated_at) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)";

const EMIT_INSERT: &str = "INSERT INTO announcements \
     (id, event, show_id, broadcast_id, caster_id, template_id, channel, payload, dedupe_key, \
      scheduled_at, status, attempts, created_at, updated_at) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?) \
     ON CONFLICT(dedupe_key) DO NOTHING";

/// Input for the store-level `emit`/`emit_tx` entry points.
pub struct NewAnnouncement<'a> {
    pub event: AnnouncementEvent,
    pub show_id: Option<&'a str>,
    pub broadcast_id: Option<&'a str>,
    pub caster_id: Option<&'a str>,
    /// `None` → resolve from `event` + `show_id` + `channel`.
    pub template_id: Option<&'a str>,
    pub channel: AnnouncementChannel,
    pub payload: serde_json::Value,
    pub dedupe_key: Option<String>,
    /// API RFC3339; `None` = send immediately (no #36 timing).
    pub scheduled_at: Option<String>,
}

fn parse_payload(raw: &str) -> ApiResult<serde_json::Value> {
    serde_json::from_str(raw)
        .map_err(|err| ApiError::Internal(anyhow::anyhow!("invalid stored payload: {err}")))
}

fn opt_api_time(raw: Option<String>) -> ApiResult<Option<String>> {
    match raw {
        Some(raw) => Ok(Some(fmt_api_time(parse_storage_time(&raw)?))),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Lists announcements, optionally filtered by `status`, `event` and the
/// `scheduled_at` window (`from` inclusive, `to` exclusive; `NULL` rows are
/// excluded by a window filter).
pub async fn list(
    pool: &SqlitePool,
    status: Option<AnnouncementStatus>,
    event: Option<AnnouncementEvent>,
    from: Option<String>,
    to: Option<String>,
) -> ApiResult<Vec<AnnouncementDto>> {
    let status = status.map(AnnouncementStatus::as_str);
    let event = event.map(AnnouncementEvent::as_str);
    let sql = format!(
        "{SELECT} WHERE (? IS NULL OR status = ?) AND (? IS NULL OR event = ?) \
         AND (? IS NULL OR scheduled_at >= ?) AND (? IS NULL OR scheduled_at < ?) \
         ORDER BY created_at, id"
    );
    let rows: Vec<AnnouncementRow> = sqlx::query_as(&sql)
        .bind(status)
        .bind(status)
        .bind(event)
        .bind(event)
        .bind(&from)
        .bind(&from)
        .bind(&to)
        .bind(&to)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(AnnouncementRow::into_dto).collect()
}

/// Fetches one announcement or returns `404`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementDto> {
    fetch(pool, id).await
}

async fn fetch(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<AnnouncementRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::not_found("announcement", id)),
    }
}

async fn fetch_conn(conn: &mut SqliteConnection, id: &str) -> ApiResult<AnnouncementDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<AnnouncementRow> = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::not_found("announcement", id)),
    }
}

async fn fetch_by_dedupe(conn: &mut SqliteConnection, key: &str) -> ApiResult<AnnouncementDto> {
    let sql = format!("{SELECT} WHERE dedupe_key = ?");
    let row: Option<AnnouncementRow> = sqlx::query_as(&sql)
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::Internal(anyhow::anyhow!(
            "dedupe collision without row for key '{key}'"
        ))),
    }
}

// ---------------------------------------------------------------------------
// CRUD (public API)
// ---------------------------------------------------------------------------

/// Creates an announcement explicitly (`POST /announcements`). A caller-given
/// `dedupe_key` that already exists is `409 duplicate_announcement`.
pub async fn create(
    pool: &SqlitePool,
    req: &CreateAnnouncementRequest,
) -> ApiResult<AnnouncementDto> {
    let event = AnnouncementEvent::parse("event", &req.event)?;
    let channel = match req.channel.as_deref() {
        None => AnnouncementChannel::Telegram,
        Some(raw) => AnnouncementChannel::parse("channel", raw)?,
    };
    let payload = req.payload.clone().unwrap_or_else(|| json!({}));
    let scheduled = match &req.scheduled_at {
        Some(raw) => Some(fmt_storage_time(parse_api_time("scheduled_at", raw)?)),
        None => None,
    };

    let mut tx = pool.begin().await?;
    ensure_ref(&mut tx, "shows", req.show_id.as_deref(), "show").await?;
    ensure_ref(
        &mut tx,
        "broadcasts",
        req.broadcast_id.as_deref(),
        "broadcast",
    )
    .await?;
    ensure_ref(&mut tx, "casters", req.caster_id.as_deref(), "caster").await?;
    let template_id = match &req.template_id {
        Some(id) => {
            ensure_ref(&mut tx, "announcement_templates", Some(id), "template").await?;
            Some(id.clone())
        }
        None => resolve_template(&mut tx, event, req.show_id.as_deref(), channel).await?,
    };

    let id = new_id();
    let ts = now_ts();
    let res = sqlx::query(INSERT)
        .bind(&id)
        .bind(event.as_str())
        .bind(req.show_id.as_deref())
        .bind(req.broadcast_id.as_deref())
        .bind(req.caster_id.as_deref())
        .bind(template_id.as_deref())
        .bind(channel.as_str())
        .bind(payload.to_string())
        .bind(req.dedupe_key.as_deref())
        .bind(scheduled.as_deref())
        .bind(&ts)
        .bind(&ts)
        .execute(&mut *tx)
        .await;

    if let Err(err) = res {
        if unique_violation(&err).is_some() {
            return Err(ApiError::Conflict {
                code: "duplicate_announcement",
                message: "an announcement with this dedupe_key already exists".to_string(),
                details: Some(json!({ "dedupe_key": req.dedupe_key })),
            });
        }
        if is_foreign_key_violation(&err) {
            return Err(ApiError::not_found("reference", "unknown"));
        }
        return Err(err.into());
    }
    tx.commit().await?;
    fetch(pool, &id).await
}

/// COALESCE patch (`PATCH /announcements/{id}`). `status` may only move to
/// `cancelled`; cancelling an already `sent`/`cancelled` row is `409`.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    req: &UpdateAnnouncementRequest,
) -> ApiResult<AnnouncementDto> {
    let current = fetch(pool, id).await?;

    let status = match req.status.as_deref() {
        None => None,
        Some("cancelled") => {
            if matches!(
                current.status,
                AnnouncementStatus::Sent | AnnouncementStatus::Cancelled
            ) {
                return Err(illegal_state(current.status));
            }
            Some(AnnouncementStatus::Cancelled)
        }
        Some(other) => {
            return Err(ApiError::validation(
                "status",
                format!("only 'cancelled' is allowed, got '{other}'"),
            ));
        }
    };

    let scheduled = match &req.scheduled_at {
        Some(raw) => Some(fmt_storage_time(parse_api_time("scheduled_at", raw)?)),
        None => None,
    };

    if let Some(template_id) = &req.template_id {
        ensure_ref_owned(pool, "announcement_templates", template_id, "template").await?;
    }

    let ts = now_ts();
    let res = sqlx::query(
        "UPDATE announcements SET \
         payload = COALESCE(?, payload), \
         template_id = COALESCE(?, template_id), \
         scheduled_at = COALESCE(?, scheduled_at), \
         status = COALESCE(?, status), \
         updated_at = ? \
         WHERE id = ?",
    )
    .bind(req.payload.as_ref().map(|value| value.to_string()))
    .bind(req.template_id.as_deref())
    .bind(scheduled.as_deref())
    .bind(status.map(AnnouncementStatus::as_str))
    .bind(&ts)
    .bind(id)
    .execute(pool)
    .await?;

    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("announcement", id));
    }
    fetch(pool, id).await
}

/// Deletes an announcement; a `sent` row is kept (`409 announcement_sent`).
pub async fn delete(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let current = fetch(pool, id).await?;
    if matches!(current.status, AnnouncementStatus::Sent) {
        return Err(ApiError::Conflict {
            code: "announcement_sent",
            message: "a sent announcement cannot be deleted".to_string(),
            details: Some(json!({ "status": current.status.as_str() })),
        });
    }
    sqlx::query("DELETE FROM announcements WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Emit (idempotent, used by every automatic source)
// ---------------------------------------------------------------------------

/// Idempotent emit in its own transaction. `Ok(None)` = no active template.
pub async fn emit(
    pool: &SqlitePool,
    new: NewAnnouncement<'_>,
) -> ApiResult<Option<AnnouncementDto>> {
    let mut tx = pool.begin().await?;
    let dto = emit_tx(&mut tx, new).await?;
    tx.commit().await?;
    Ok(dto)
}

/// Idempotent emit inside a caller-supplied transaction (broadcast hook).
///
/// Uses `ON CONFLICT(dedupe_key) DO NOTHING`; on a collision the **existing**
/// row is returned (`created = false` semantics), never an error.
pub async fn emit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    new: NewAnnouncement<'_>,
) -> ApiResult<Option<AnnouncementDto>> {
    let template_id = match new.template_id {
        Some(id) => Some(id.to_string()),
        None => resolve_template(&mut *tx, new.event, new.show_id, new.channel).await?,
    };
    let Some(template_id) = template_id else {
        return Ok(None);
    };

    let scheduled = match &new.scheduled_at {
        Some(raw) => Some(fmt_storage_time(parse_api_time("scheduled_at", raw)?)),
        None => None,
    };
    let id = new_id();
    let ts = now_ts();
    let res = sqlx::query(EMIT_INSERT)
        .bind(&id)
        .bind(new.event.as_str())
        .bind(new.show_id)
        .bind(new.broadcast_id)
        .bind(new.caster_id)
        .bind(&template_id)
        .bind(new.channel.as_str())
        .bind(new.payload.to_string())
        .bind(new.dedupe_key.as_deref())
        .bind(scheduled.as_deref())
        .bind(&ts)
        .bind(&ts)
        .execute(&mut **tx)
        .await?;

    let dto = if res.rows_affected() == 1 {
        fetch_conn(tx, &id).await?
    } else {
        match new.dedupe_key.as_deref() {
            Some(key) => fetch_by_dedupe(tx, key).await?,
            None => fetch_conn(tx, &id).await?,
        }
    };
    Ok(Some(dto))
}

/// Resolves the template for `(event, show_id, channel)`: a per-show override
/// wins over the global default; only `enabled = 1` templates participate.
pub async fn resolve_template(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: AnnouncementEvent,
    show_id: Option<&str>,
    channel: AnnouncementChannel,
) -> ApiResult<Option<String>> {
    if let Some(show_id) = show_id {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM announcement_templates \
             WHERE event = ? AND channel = ? AND show_id = ? AND enabled = 1 \
             ORDER BY created_at, id LIMIT 1",
        )
        .bind(event.as_str())
        .bind(channel.as_str())
        .bind(show_id)
        .fetch_optional(&mut **tx)
        .await?;
        if id.is_some() {
            return Ok(id);
        }
    }
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM announcement_templates \
         WHERE event = ? AND channel = ? AND show_id IS NULL AND enabled = 1 \
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(event.as_str())
    .bind(channel.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Event-source seams (#36 / later wiring fills the call sites)
// ---------------------------------------------------------------------------

/// Seam: `live_on` / `live_off`, emitted from the broadcast transition hook.
/// `scheduled_at` (API time) carries the configured delay; `None` = due now.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn emit_live_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: AnnouncementEvent,
    broadcast_id: &str,
    show_id: Option<&str>,
    caster_id: &str,
    start_time: Option<String>,
    end_time: Option<String>,
    scheduled_at: Option<String>,
) -> ApiResult<Option<AnnouncementDto>> {
    let show_name: Option<String> = match show_id {
        Some(show_id) => {
            sqlx::query_scalar("SELECT name FROM shows WHERE id = ?")
                .bind(show_id)
                .fetch_optional(&mut **tx)
                .await?
        }
        None => None,
    };
    let caster_name: Option<String> =
        sqlx::query_scalar("SELECT display_name FROM casters WHERE id = ?")
            .bind(caster_id)
            .fetch_optional(&mut **tx)
            .await?;

    let ctx = events::PayloadContext {
        broadcast_id: Some(broadcast_id.to_string()),
        show_id: show_id.map(str::to_string),
        show_name,
        caster_id: Some(caster_id.to_string()),
        caster_name,
        start_time,
        end_time,
        ..Default::default()
    };
    let dedupe = events::dedupe_key(
        event,
        &events::DedupeContext {
            broadcast_id: Some(broadcast_id),
            ..Default::default()
        },
    );
    emit_tx(
        tx,
        NewAnnouncement {
            event,
            show_id,
            broadcast_id: Some(broadcast_id),
            caster_id: Some(caster_id),
            template_id: None,
            channel: AnnouncementChannel::Telegram,
            payload: events::build_payload(event, &ctx),
            dedupe_key: dedupe,
            scheduled_at,
        },
    )
    .await
}

/// Seam: `recording_published` — `dedupe_key = tape:<id>` (architecture §3.6).
pub async fn emit_recording_published(
    pool: &SqlitePool,
    tape_id: &str,
    show_id: Option<&str>,
    caster_id: Option<&str>,
) -> ApiResult<Option<AnnouncementDto>> {
    let ctx = events::PayloadContext {
        tape_id: Some(tape_id.to_string()),
        show_id: show_id.map(str::to_string),
        caster_id: caster_id.map(str::to_string),
        ..Default::default()
    };
    emit(
        pool,
        NewAnnouncement {
            event: AnnouncementEvent::RecordingPublished,
            show_id,
            broadcast_id: None,
            caster_id,
            template_id: None,
            channel: AnnouncementChannel::Telegram,
            payload: events::build_payload(AnnouncementEvent::RecordingPublished, &ctx),
            dedupe_key: events::dedupe_key(
                AnnouncementEvent::RecordingPublished,
                &events::DedupeContext {
                    tape_id: Some(tape_id),
                    ..Default::default()
                },
            ),
            scheduled_at: None,
        },
    )
    .await
}

/// Seam: `fallback_entered` — caller supplies a monotonic `epoch`.
pub async fn emit_fallback_entered(
    pool: &SqlitePool,
    epoch: &str,
    caster_id: Option<&str>,
) -> ApiResult<Option<AnnouncementDto>> {
    let ctx = events::PayloadContext {
        epoch: Some(epoch.to_string()),
        caster_id: caster_id.map(str::to_string),
        ..Default::default()
    };
    emit(
        pool,
        NewAnnouncement {
            event: AnnouncementEvent::FallbackEntered,
            show_id: None,
            broadcast_id: None,
            caster_id,
            template_id: None,
            channel: AnnouncementChannel::Telegram,
            payload: events::build_payload(AnnouncementEvent::FallbackEntered, &ctx),
            dedupe_key: events::dedupe_key(
                AnnouncementEvent::FallbackEntered,
                &events::DedupeContext {
                    epoch: Some(epoch),
                    ..Default::default()
                },
            ),
            scheduled_at: None,
        },
    )
    .await
}

/// Seam: `error` — `dedupe_key = error:<scope>:<epoch>`.
pub async fn emit_error(
    pool: &SqlitePool,
    scope: &str,
    epoch: &str,
    message: Option<&str>,
) -> ApiResult<Option<AnnouncementDto>> {
    let ctx = events::PayloadContext {
        scope: Some(scope.to_string()),
        epoch: Some(epoch.to_string()),
        message: message.map(str::to_string),
        ..Default::default()
    };
    emit(
        pool,
        NewAnnouncement {
            event: AnnouncementEvent::Error,
            show_id: None,
            broadcast_id: None,
            caster_id: None,
            template_id: None,
            channel: AnnouncementChannel::Telegram,
            payload: events::build_payload(AnnouncementEvent::Error, &ctx),
            dedupe_key: events::dedupe_key(
                AnnouncementEvent::Error,
                &events::DedupeContext {
                    scope: Some(scope),
                    epoch: Some(epoch),
                    ..Default::default()
                },
            ),
            scheduled_at: None,
        },
    )
    .await
}

/// Seam: `show_reminder` — timing/`scheduled_at` is decided by #36.
pub async fn emit_show_reminder(
    pool: &SqlitePool,
    broadcast_id: &str,
    scheduled_at: Option<&str>,
    caster_id: Option<&str>,
) -> ApiResult<Option<AnnouncementDto>> {
    let ctx = events::PayloadContext {
        broadcast_id: Some(broadcast_id.to_string()),
        caster_id: caster_id.map(str::to_string),
        ..Default::default()
    };
    emit(
        pool,
        NewAnnouncement {
            event: AnnouncementEvent::ShowReminder,
            show_id: None,
            broadcast_id: Some(broadcast_id),
            caster_id,
            template_id: None,
            channel: AnnouncementChannel::Telegram,
            payload: events::build_payload(AnnouncementEvent::ShowReminder, &ctx),
            dedupe_key: events::dedupe_key(
                AnnouncementEvent::ShowReminder,
                &events::DedupeContext {
                    broadcast_id: Some(broadcast_id),
                    ..Default::default()
                },
            ),
            scheduled_at: scheduled_at.map(str::to_string),
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// show_reminder planning inside a caller transaction (#36)
// ---------------------------------------------------------------------------

/// Everything the reminder emitter needs beyond the resolved timing.
pub(crate) struct ShowReminderInput<'a> {
    pub broadcast_id: &'a str,
    pub show_id: Option<&'a str>,
    pub show_name: Option<&'a str>,
    pub caster_id: Option<&'a str>,
    pub starts_at: DateTime<Utc>,
    /// `None` = immediately due.
    pub scheduled_at: Option<DateTime<Utc>>,
}

fn show_reminder_dedupe(broadcast_id: &str) -> Option<String> {
    events::dedupe_key(
        AnnouncementEvent::ShowReminder,
        &events::DedupeContext {
            broadcast_id: Some(broadcast_id),
            ..Default::default()
        },
    )
}

fn show_reminder_payload(input: &ShowReminderInput<'_>) -> serde_json::Value {
    let ctx = events::PayloadContext {
        broadcast_id: Some(input.broadcast_id.to_string()),
        show_id: input.show_id.map(str::to_string),
        show_name: input.show_name.map(str::to_string),
        start_time: Some(fmt_api_time(input.starts_at)),
        ..Default::default()
    };
    events::build_payload(AnnouncementEvent::ShowReminder, &ctx)
}

/// Idempotent `show_reminder` emit inside a caller transaction (dedupe key
/// `show_reminder:<broadcast_id>`); carries `show_id`/`show_name`/`start_time`
/// so per-show templates resolve (unlike the legacy pool seam).
pub(crate) async fn emit_show_reminder_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: ShowReminderInput<'_>,
) -> ApiResult<Option<AnnouncementDto>> {
    let scheduled = input.scheduled_at.map(fmt_api_time);
    emit_tx(
        tx,
        NewAnnouncement {
            event: AnnouncementEvent::ShowReminder,
            show_id: input.show_id,
            broadcast_id: Some(input.broadcast_id),
            caster_id: input.caster_id,
            template_id: None,
            channel: AnnouncementChannel::Telegram,
            payload: show_reminder_payload(&input),
            dedupe_key: show_reminder_dedupe(input.broadcast_id),
            scheduled_at: scheduled,
        },
    )
    .await
}

/// Reschedules an existing pending reminder (a `UPDATE`, never a second
/// `INSERT`). When no pending row exists (`kind`-switch, cancelled) a fresh
/// reminder is emitted.
pub(crate) async fn reschedule_show_reminder_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: ShowReminderInput<'_>,
) -> ApiResult<Option<AnnouncementDto>> {
    let Some(key) = show_reminder_dedupe(input.broadcast_id) else {
        return Ok(None);
    };
    let scheduled = input.scheduled_at.map(fmt_storage_time);
    let ts = now_ts();
    let res = sqlx::query(
        "UPDATE announcements SET scheduled_at = ?, payload = ?, updated_at = ? \
         WHERE dedupe_key = ? AND status = 'pending'",
    )
    .bind(scheduled.as_deref())
    .bind(show_reminder_payload(&input).to_string())
    .bind(&ts)
    .bind(&key)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return emit_show_reminder_tx(tx, input).await;
    }
    Ok(None)
}

/// Cancels the pending reminder for `broadcast_id` (idempotent no-op when
/// none is pending).
pub(crate) async fn cancel_show_reminder_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    broadcast_id: &str,
) -> ApiResult<()> {
    let Some(key) = show_reminder_dedupe(broadcast_id) else {
        return Ok(());
    };
    let ts = now_ts();
    sqlx::query(
        "UPDATE announcements SET status = 'cancelled', updated_at = ? \
         WHERE dedupe_key = ? AND status = 'pending'",
    )
    .bind(&ts)
    .bind(&key)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Retry state machine
// ---------------------------------------------------------------------------

/// `POST /announcements/{id}/send` — re-arm the queue (never sends).
///
/// `pending`/`failed` → `pending` with `next_attempt_at = now`; `pending`
/// stays idempotent. `sent`/`cancelled` → `409 illegal_state`.
pub async fn send(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementDto> {
    let current = fetch(pool, id).await?;
    match current.status {
        AnnouncementStatus::Pending | AnnouncementStatus::Failed => {
            let ts = now_ts();
            sqlx::query(
                "UPDATE announcements SET status = 'pending', next_attempt_at = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(&ts)
            .bind(&ts)
            .bind(id)
            .execute(pool)
            .await?;
            fetch(pool, id).await
        }
        AnnouncementStatus::Sent | AnnouncementStatus::Cancelled => {
            Err(illegal_state(current.status))
        }
    }
}

/// Marks an announcement as sent (#37 calls this after a successful push).
pub async fn mark_sent(
    pool: &SqlitePool,
    id: &str,
    at: DateTime<Utc>,
) -> ApiResult<AnnouncementDto> {
    let mut tx = pool.begin().await?;
    let ts = now_ts();
    let res = sqlx::query(
        "UPDATE announcements SET status = 'sent', sent_at = ?, error = NULL, \
         next_attempt_at = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(fmt_storage_time(at))
    .bind(&ts)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("announcement", id));
    }
    crate::store::broadcasts::insert_event_log(
        &mut *tx,
        "announcement_sent",
        None,
        None,
        None,
        json!({ "announcement_id": id }),
    )
    .await?;
    tx.commit().await?;
    fetch(pool, id).await
}

/// Marks a failed attempt (#37 calls this after a transport error).
///
/// `attempts += 1`; below [`retry::MAX_ATTEMPTS`] the row returns to
/// `pending` with `next_attempt_at = at + backoff(attempts)`, at the cap it
/// becomes terminal (`failed`).
pub async fn mark_failed(
    pool: &SqlitePool,
    id: &str,
    error: &str,
    at: DateTime<Utc>,
) -> ApiResult<AnnouncementDto> {
    let mut tx = pool.begin().await?;
    let current = fetch_conn(&mut tx, id).await?;
    let attempts = current.attempts + 1;
    let (status, next) = if attempts >= retry::MAX_ATTEMPTS {
        (AnnouncementStatus::Failed, None)
    } else {
        (
            AnnouncementStatus::Pending,
            Some(fmt_storage_time(at + retry::backoff(attempts))),
        )
    };
    let ts = now_ts();
    sqlx::query(
        "UPDATE announcements SET status = ?, attempts = ?, next_attempt_at = ?, error = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(attempts)
    .bind(next.as_deref())
    .bind(error)
    .bind(&ts)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    crate::store::broadcasts::insert_event_log(
        &mut *tx,
        "announcement_failed",
        None,
        None,
        None,
        json!({ "announcement_id": id, "attempts": attempts, "error": error }),
    )
    .await?;
    tx.commit().await?;
    fetch(pool, id).await
}

/// Queue for #37: due `pending` rows (`next_attempt_at <= now` or unset).
pub async fn due(pool: &SqlitePool, now: DateTime<Utc>) -> ApiResult<Vec<AnnouncementDto>> {
    let now = fmt_storage_time(now);
    let sql = format!(
        "{SELECT} WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?) \
         ORDER BY next_attempt_at, id"
    );
    let rows: Vec<AnnouncementRow> = sqlx::query_as(&sql).bind(&now).fetch_all(pool).await?;
    rows.into_iter().map(AnnouncementRow::into_dto).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn illegal_state(status: AnnouncementStatus) -> ApiError {
    ApiError::Conflict {
        code: "illegal_state",
        message: format!("announcement is in status '{}'", status.as_str()),
        details: Some(json!({ "status": status.as_str() })),
    }
}

async fn ensure_ref(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &str,
    id: Option<&str>,
    resource: &'static str,
) -> ApiResult<()> {
    let Some(id) = id else { return Ok(()) };
    let exists: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE id = ?"))
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found(resource, id.to_string()));
    }
    Ok(())
}

async fn ensure_ref_owned(
    pool: &SqlitePool,
    table: &str,
    id: &str,
    resource: &'static str,
) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE id = ?"))
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found(resource, id.to_string()));
    }
    Ok(())
}
