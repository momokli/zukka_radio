//! Broadcast store: CRUD, status transitions, conflict detection and the
//! append-only `event_log` writes (Issue #11).

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use sqlx::sqlite::Sqlite;

use crate::dto::{
    BroadcastDto, BroadcastKind, BroadcastStatus, CreateBroadcastRequest, UpdateBroadcastRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;
use crate::store::schedule::{Slot, detect_conflicts};
use crate::time::{fmt_api_time, fmt_storage_time, parse_api_time, parse_storage_time};
use crate::uuid::new_id;

/// Row shape of `broadcasts` (timestamps still in storage form).
#[derive(Debug, sqlx::FromRow)]
struct BroadcastRow {
    id: String,
    show_id: Option<String>,
    caster_id: String,
    kind: BroadcastKind,
    title: String,
    description: String,
    starts_at: String,
    ends_at: Option<String>,
    status: BroadcastStatus,
    tape_id: Option<String>,
    queue_id: Option<String>,
    version: i64,
    created_by: String,
    created_at: String,
    updated_at: String,
}

impl BroadcastRow {
    fn into_dto(self) -> ApiResult<BroadcastDto> {
        Ok(BroadcastDto {
            id: self.id,
            show_id: self.show_id,
            caster_id: self.caster_id,
            kind: self.kind,
            title: self.title,
            description: self.description,
            starts_at: fmt_api_time(parse_storage_time(&self.starts_at)?),
            ends_at: match &self.ends_at {
                Some(raw) => Some(fmt_api_time(parse_storage_time(raw)?)),
                None => None,
            },
            status: self.status,
            tape_id: self.tape_id,
            queue_id: self.queue_id,
            version: self.version,
            created_by: self.created_by,
            created_at: self.created_at,
            updated_at: self.updated_at,
            warnings: Vec::new(),
        })
    }
}

const SELECT: &str = "SELECT id, show_id, caster_id, kind, title, description, starts_at, ends_at, \
     status, tape_id, queue_id, version, created_by, created_at, updated_at FROM broadcasts";

/// Lists broadcasts, optionally filtered by window, caster and status.
///
/// The window filter uses interval overlap: open ends count as `+∞`.
pub async fn list(
    pool: &SqlitePool,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    caster_id: Option<&str>,
    status: Option<BroadcastStatus>,
) -> ApiResult<Vec<BroadcastDto>> {
    let from = from.map(fmt_storage_time);
    let to = to.map(fmt_storage_time);
    let status = status.map(BroadcastStatus::as_str);
    let sql = format!(
        "{SELECT} WHERE (? IS NULL OR caster_id = ?) AND (? IS NULL OR status = ?) \
         AND (? IS NULL OR ends_at IS NULL OR ends_at > ?) \
         AND (? IS NULL OR starts_at < ?) \
         ORDER BY starts_at, id"
    );
    let rows: Vec<BroadcastRow> = sqlx::query_as(&sql)
        .bind(caster_id)
        .bind(caster_id)
        .bind(status)
        .bind(status)
        .bind(&from)
        .bind(&from)
        .bind(&to)
        .bind(&to)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(BroadcastRow::into_dto).collect()
}

/// Fetches one broadcast (without warnings) or returns `404`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<BroadcastDto> {
    fetch(pool, id).await
}

async fn fetch(pool: &SqlitePool, id: &str) -> ApiResult<BroadcastDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<BroadcastRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::not_found("broadcast", id)),
    }
}

/// Fetches a broadcast and decorates it with the current soft warnings.
pub async fn get_with_warnings(pool: &SqlitePool, id: &str) -> ApiResult<BroadcastDto> {
    let mut dto = fetch(pool, id).await?;
    dto.warnings = warnings_for_dto(pool, &dto).await?;
    Ok(dto)
}

pub(crate) async fn warnings_for_dto(
    pool: &SqlitePool,
    dto: &BroadcastDto,
) -> ApiResult<Vec<crate::dto::WarningDto>> {
    let starts_at = parse_api_time("starts_at", &dto.starts_at)?;
    let ends_at = match &dto.ends_at {
        Some(raw) => Some(parse_api_time("ends_at", raw)?),
        None => None,
    };
    let slot = Slot {
        id: Some(&dto.id),
        show_id: dto.show_id.as_deref(),
        caster_id: &dto.caster_id,
        kind: dto.kind,
        starts_at,
        ends_at,
    };
    let mut conn = pool.acquire().await?;
    Ok(detect_conflicts(&mut conn, &slot).await?.warnings)
}

/// Creates a broadcast; returns it with the §4.2 warnings.
pub async fn create(pool: &SqlitePool, req: &CreateBroadcastRequest) -> ApiResult<BroadcastDto> {
    let kind = BroadcastKind::parse("kind", &req.kind)?;
    let title = non_empty("title", &req.title)?;
    let starts_at = parse_api_time("starts_at", &req.starts_at)?;
    let ends_at = match &req.ends_at {
        Some(raw) => Some(parse_api_time("ends_at", raw)?),
        None => None,
    };
    validate_window(starts_at, ends_at)?;
    validate_content(kind, req.tape_id.as_deref(), req.queue_id.as_deref())?;

    ensure_exists(pool, "casters", &req.caster_id, "caster").await?;
    if let Some(show_id) = &req.show_id {
        ensure_exists(pool, "shows", show_id, "show").await?;
    }
    if let Some(tape_id) = &req.tape_id {
        ensure_exists(pool, "tapes", tape_id, "tape").await?;
    }
    if let Some(queue_id) = &req.queue_id {
        ensure_exists(pool, "queues", queue_id, "queue").await?;
    }
    let created_by = req
        .created_by
        .clone()
        .unwrap_or_else(|| req.caster_id.clone());
    ensure_exists(pool, "casters", &created_by, "caster").await?;

    let id = new_id();
    let ts = now_ts();
    let starts_storage = fmt_storage_time(starts_at);
    let ends_storage = ends_at.map(fmt_storage_time);

    let mut tx = pool.begin().await?;
    let report = detect_conflicts(
        &mut tx,
        &Slot {
            id: None,
            show_id: req.show_id.as_deref(),
            caster_id: &req.caster_id,
            kind,
            starts_at,
            ends_at,
        },
    )
    .await?;
    if let Some((show_id, conflicting)) = report.show_overlap {
        return Err(show_overlap_error(show_id, conflicting));
    }

    sqlx::query(
        "INSERT INTO broadcasts \
         (id, show_id, caster_id, kind, title, description, starts_at, ends_at, status, tape_id, \
          queue_id, version, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'scheduled', ?, ?, 1, ?, ?, ?)",
    )
    .bind(&id)
    .bind(req.show_id.as_deref())
    .bind(&req.caster_id)
    .bind(kind.as_str())
    .bind(&title)
    .bind(req.description.clone().unwrap_or_default())
    .bind(&starts_storage)
    .bind(ends_storage.as_deref())
    .bind(req.tape_id.as_deref())
    .bind(req.queue_id.as_deref())
    .bind(&created_by)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut *tx)
    .await?;

    insert_event_log(
        &mut *tx,
        "broadcast_created",
        Some(&created_by),
        Some(&id),
        Some(&req.caster_id),
        json!({ "version": 1, "kind": kind.as_str() }),
    )
    .await?;

    tx.commit().await?;

    get_with_warnings(pool, &id).await
}

/// Updates a broadcast with optimistic locking on `version`.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    req: &UpdateBroadcastRequest,
) -> ApiResult<BroadcastDto> {
    let version = req
        .version
        .ok_or_else(|| ApiError::validation("version", "version is required"))?;
    let current = fetch(pool, id).await?;

    let show_id = match &req.show_id {
        Some(value) => value.clone(),
        None => current.show_id.clone(),
    };
    let caster_id = req
        .caster_id
        .clone()
        .unwrap_or_else(|| current.caster_id.clone());
    let kind = match &req.kind {
        Some(raw) => BroadcastKind::parse("kind", raw)?,
        None => current.kind,
    };
    let title = match &req.title {
        Some(raw) => non_empty("title", raw)?,
        None => current.title.clone(),
    };
    let starts_at = match &req.starts_at {
        Some(raw) => parse_api_time("starts_at", raw)?,
        None => parse_api_time("starts_at", &current.starts_at)?,
    };
    let ends_at = match &req.ends_at {
        Some(value) => match value {
            Some(raw) => Some(parse_api_time("ends_at", raw)?),
            None => None,
        },
        None => match &current.ends_at {
            Some(raw) => Some(parse_api_time("ends_at", raw)?),
            None => None,
        },
    };
    let tape_id = match &req.tape_id {
        Some(value) => value.clone(),
        None => current.tape_id.clone(),
    };
    let queue_id = match &req.queue_id {
        Some(value) => value.clone(),
        None => current.queue_id.clone(),
    };

    validate_window(starts_at, ends_at)?;
    validate_content(kind, tape_id.as_deref(), queue_id.as_deref())?;

    ensure_exists(pool, "casters", &caster_id, "caster").await?;
    if let Some(show_id) = &show_id {
        ensure_exists(pool, "shows", show_id, "show").await?;
    }
    if let Some(tape_id) = &tape_id {
        ensure_exists(pool, "tapes", tape_id, "tape").await?;
    }
    if let Some(queue_id) = &queue_id {
        ensure_exists(pool, "queues", queue_id, "queue").await?;
    }

    let ts = now_ts();
    let starts_storage = fmt_storage_time(starts_at);
    let ends_storage = ends_at.map(fmt_storage_time);

    let mut tx = pool.begin().await?;
    let report = detect_conflicts(
        &mut tx,
        &Slot {
            id: Some(id),
            show_id: show_id.as_deref(),
            caster_id: &caster_id,
            kind,
            starts_at,
            ends_at,
        },
    )
    .await?;
    if let Some((overlap_show_id, conflicting)) = report.show_overlap {
        return Err(show_overlap_error(overlap_show_id, conflicting));
    }

    let res = sqlx::query(
        "UPDATE broadcasts SET \
         show_id = ?, caster_id = ?, kind = ?, title = ?, \
         description = COALESCE(?, description), starts_at = ?, ends_at = ?, \
         tape_id = ?, queue_id = ?, version = version + 1, updated_at = ? \
         WHERE id = ? AND version = ?",
    )
    .bind(show_id.as_deref())
    .bind(&caster_id)
    .bind(kind.as_str())
    .bind(&title)
    .bind(req.description.clone())
    .bind(&starts_storage)
    .bind(ends_storage.as_deref())
    .bind(tape_id.as_deref())
    .bind(queue_id.as_deref())
    .bind(&ts)
    .bind(id)
    .bind(version)
    .execute(&mut *tx)
    .await?;

    if res.rows_affected() == 0 {
        return Err(stale_or_missing(pool, id).await);
    }

    insert_event_log(
        &mut *tx,
        "broadcast_updated",
        Some(&current.created_by),
        Some(id),
        Some(&caster_id),
        json!({ "version": version + 1, "kind": kind.as_str() }),
    )
    .await?;

    tx.commit().await?;

    get_with_warnings(pool, id).await
}

/// Deletes a broadcast (`tapes.broadcast_id` becomes `NULL` via FK).
pub async fn delete(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let res = sqlx::query("DELETE FROM broadcasts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("broadcast", id));
    }
    Ok(())
}

/// `POST /broadcasts/{id}/start` — scheduled → live.
pub async fn start(pool: &SqlitePool, id: &str) -> ApiResult<BroadcastDto> {
    transition(pool, id, Action::Start).await
}

/// `POST /broadcasts/{id}/end` — live → ended, stamping `ends_at = now`.
pub async fn end(pool: &SqlitePool, id: &str) -> ApiResult<BroadcastDto> {
    transition(pool, id, Action::End).await
}

/// `POST /broadcasts/{id}/cancel` — scheduled/live → cancelled.
pub async fn cancel(pool: &SqlitePool, id: &str) -> ApiResult<BroadcastDto> {
    transition(pool, id, Action::Cancel).await
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Start,
    End,
    Cancel,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::End => "end",
            Action::Cancel => "cancel",
        }
    }

    fn event(self) -> &'static str {
        match self {
            Action::Start => "broadcast_started",
            Action::End => "broadcast_ended",
            Action::Cancel => "broadcast_cancelled",
        }
    }
}

async fn transition(pool: &SqlitePool, id: &str, action: Action) -> ApiResult<BroadcastDto> {
    let mut tx = pool.begin().await?;

    #[derive(sqlx::FromRow)]
    struct Current {
        status: BroadcastStatus,
        starts_at: String,
    }

    let current: Option<Current> =
        sqlx::query_as("SELECT status, starts_at FROM broadcasts WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Err(ApiError::not_found("broadcast", id));
    };

    let next = match (action, current.status) {
        (Action::Start, BroadcastStatus::Scheduled) => BroadcastStatus::Live,
        (Action::End, BroadcastStatus::Live) => BroadcastStatus::Ended,
        (Action::Cancel, BroadcastStatus::Scheduled | BroadcastStatus::Live) => {
            BroadcastStatus::Cancelled
        }
        (_, from) => return Err(illegal_transition(from, action.name())),
    };

    // `end` stamps ends_at = now (never before starts_at, schema CHECK).
    let new_end = if matches!(action, Action::End) {
        let starts = parse_storage_time(&current.starts_at)?;
        let now = Utc::now();
        let end = if now > starts {
            now
        } else {
            starts + Duration::seconds(1)
        };
        Some(fmt_storage_time(end))
    } else {
        None
    };

    let ts = now_ts();
    sqlx::query(
        "UPDATE broadcasts SET status = ?, version = version + 1, updated_at = ?, \
         ends_at = COALESCE(?, ends_at) WHERE id = ?",
    )
    .bind(next.as_str())
    .bind(&ts)
    .bind(new_end.as_deref())
    .bind(id)
    .execute(&mut *tx)
    .await?;

    insert_event_log(
        &mut *tx,
        action.event(),
        None,
        Some(id),
        None,
        json!({ "status": next.as_str() }),
    )
    .await?;

    tx.commit().await?;

    get_with_warnings(pool, id).await
}

/// Inserts one append-only `event_log` row.
///
/// Accepts a pool or a transaction (`&mut tx`) as executor.
pub(crate) async fn insert_event_log(
    executor: impl sqlx::Executor<'_, Database = Sqlite>,
    event: &'static str,
    actor_caster_id: Option<&str>,
    broadcast_id: Option<&str>,
    caster_id: Option<&str>,
    details: serde_json::Value,
) -> ApiResult<()> {
    let id = new_id();
    let ts = now_ts();
    sqlx::query(
        "INSERT INTO event_log \
         (id, occurred_at, event, actor_caster_id, broadcast_id, caster_id, source_layer, details, \
          created_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(&ts)
    .bind(event)
    .bind(actor_caster_id)
    .bind(broadcast_id)
    .bind(caster_id)
    .bind(details.to_string())
    .bind(&ts)
    .execute(executor)
    .await?;
    Ok(())
}

fn non_empty(field: &'static str, raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::validation(
            field,
            format!("{field} must not be empty"),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_window(starts_at: DateTime<Utc>, ends_at: Option<DateTime<Utc>>) -> ApiResult<()> {
    if let Some(end) = ends_at {
        if end <= starts_at {
            return Err(ApiError::validation(
                "ends_at",
                "ends_at must be after starts_at",
            ));
        }
    }
    Ok(())
}

/// Enforces the kind/content matrix from §3.3 in Rust (→ `422`, not a DB error).
fn validate_content(
    kind: BroadcastKind,
    tape_id: Option<&str>,
    queue_id: Option<&str>,
) -> ApiResult<()> {
    match kind {
        BroadcastKind::Live => {
            if tape_id.is_some() {
                return Err(ApiError::validation(
                    "tape_id",
                    "kind 'live' must not reference a tape",
                ));
            }
            if queue_id.is_some() {
                return Err(ApiError::validation(
                    "queue_id",
                    "kind 'live' must not reference a queue",
                ));
            }
        }
        BroadcastKind::Recording => {
            if tape_id.is_none() {
                return Err(ApiError::validation(
                    "tape_id",
                    "kind 'recording' requires a tape_id",
                ));
            }
            if queue_id.is_some() {
                return Err(ApiError::validation(
                    "queue_id",
                    "kind 'recording' must not reference a queue",
                ));
            }
        }
        BroadcastKind::Playlist => {
            if queue_id.is_none() {
                return Err(ApiError::validation(
                    "queue_id",
                    "kind 'playlist' requires a queue_id",
                ));
            }
            if tape_id.is_some() {
                return Err(ApiError::validation(
                    "tape_id",
                    "kind 'playlist' must not reference a tape",
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_exists(
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

fn show_overlap_error(show_id: String, broadcast_id: String) -> ApiError {
    ApiError::Conflict {
        code: "show_overlap",
        message: "this show already has a broadcast in the requested time slot".to_string(),
        details: Some(json!({
            "show_id": show_id,
            "conflicting_broadcast_id": broadcast_id,
        })),
    }
}

fn illegal_transition(from: BroadcastStatus, action: &'static str) -> ApiError {
    ApiError::Conflict {
        code: "illegal_transition",
        message: format!("cannot {action} a broadcast in status '{}'", from.as_str()),
        details: Some(json!({ "from": from.as_str(), "action": action })),
    }
}

async fn stale_or_missing(pool: &SqlitePool, id: &str) -> ApiError {
    let exists: Option<i64> = sqlx::query_scalar("SELECT version FROM broadcasts WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);
    match exists {
        Some(_) => ApiError::conflict(
            "version_conflict",
            "broadcast was modified by another client",
        ),
        None => ApiError::not_found("broadcast", id),
    }
}
