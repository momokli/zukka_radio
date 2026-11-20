//! Queue store: read/write queries for `queues` (+ reorder/clear).

use sqlx::sqlite::Sqlite;
use sqlx::{SqlitePool, Transaction};

use crate::dto::{CreateQueueRequest, QueueDto, QueueKind, ReorderRequest, UpdateQueueRequest};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
use crate::store::now_ts;
use crate::uuid::new_id;

/// Row shape of `queues`, mapped straight onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct QueueRow {
    id: String,
    caster_id: String,
    name: String,
    description: String,
    kind: QueueKind,
    version: i64,
    created_at: String,
    updated_at: String,
}

impl From<QueueRow> for QueueDto {
    fn from(row: QueueRow) -> Self {
        QueueDto {
            id: row.id,
            caster_id: row.caster_id,
            name: row.name,
            description: row.description,
            kind: row.kind,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

const SELECT: &str =
    "SELECT id, caster_id, name, description, kind, version, created_at, updated_at FROM queues";

/// Lists queues, optionally filtered by `caster_id` and/or `kind`.
pub async fn list(
    pool: &SqlitePool,
    caster_id: Option<&str>,
    kind: Option<QueueKind>,
) -> ApiResult<Vec<QueueDto>> {
    let kind = kind.map(QueueKind::as_str);
    let sql = format!(
        "{SELECT} WHERE (? IS NULL OR caster_id = ?) AND (? IS NULL OR kind = ?) \
         ORDER BY created_at, id"
    );
    let rows: Vec<QueueRow> = sqlx::query_as(&sql)
        .bind(caster_id)
        .bind(caster_id)
        .bind(kind)
        .bind(kind)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(QueueDto::from).collect())
}

/// Fetches one queue or returns `404`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<QueueDto> {
    fetch(pool, id).await
}

async fn fetch(pool: &SqlitePool, id: &str) -> ApiResult<QueueDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<QueueRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    row.map(QueueDto::from)
        .ok_or_else(|| ApiError::not_found("queue", id))
}

async fn fetch_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> ApiResult<QueueDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<QueueRow> = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(QueueDto::from)
        .ok_or_else(|| ApiError::not_found("queue", id))
}

/// Creates a queue: `version = 1`, `id = new_id()` (§3.1).
pub async fn create(pool: &SqlitePool, req: &CreateQueueRequest) -> ApiResult<QueueDto> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("name", "name must not be empty"));
    }
    let kind = match &req.kind {
        Some(raw) => QueueKind::parse("kind", raw)?,
        None => QueueKind::Background,
    };

    // Unknown caster_id (FK) → 404 (the endpoint matrix lists 404 for POST).
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM casters WHERE id = ?")
        .bind(&req.caster_id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("caster", req.caster_id.clone()));
    }

    let id = new_id();
    let ts = now_ts();
    let description = req.description.clone().unwrap_or_default();

    let res = sqlx::query(
        "INSERT INTO queues (id, caster_id, name, description, kind, version, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&req.caster_id)
    .bind(name)
    .bind(&description)
    .bind(kind.as_str())
    .bind(&ts)
    .bind(&ts)
    .execute(pool)
    .await;

    if let Err(err) = res {
        if let Some(msg) = unique_violation(&err) {
            if msg.contains("queues") {
                return Err(ApiError::conflict(
                    "duplicate_name",
                    "a queue with this name already exists for this caster",
                ));
            }
        }
        if is_foreign_key_violation(&err) {
            return Err(ApiError::not_found("caster", req.caster_id.clone()));
        }
        return Err(err.into());
    }

    fetch(pool, &id).await
}

/// Updates a queue with optimistic locking on `version`.
pub async fn update(pool: &SqlitePool, id: &str, req: &UpdateQueueRequest) -> ApiResult<QueueDto> {
    let version = req
        .version
        .ok_or_else(|| ApiError::validation("version", "version is required"))?;

    let name = match &req.name {
        Some(name) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(ApiError::validation("name", "name must not be empty"));
            }
            Some(trimmed.to_string())
        }
        None => None,
    };
    let kind = match &req.kind {
        Some(raw) => Some(QueueKind::parse("kind", raw)?),
        None => None,
    };

    let ts = now_ts();
    let sql = "UPDATE queues SET \
               name = COALESCE(?, name), \
               description = COALESCE(?, description), \
               kind = COALESCE(?, kind), \
               version = version + 1, \
               updated_at = ? \
               WHERE id = ? AND version = ?";
    let res = sqlx::query(sql)
        .bind(name)
        .bind(req.description.clone())
        .bind(kind.map(QueueKind::as_str))
        .bind(&ts)
        .bind(id)
        .bind(version)
        .execute(pool)
        .await;

    let res = match res {
        Ok(res) => res,
        Err(err) => {
            if let Some(msg) = unique_violation(&err) {
                if msg.contains("queues") {
                    return Err(ApiError::conflict(
                        "duplicate_name",
                        "a queue with this name already exists for this caster",
                    ));
                }
            }
            return Err(err.into());
        }
    };

    if res.rows_affected() == 0 {
        return Err(stale_or_missing(pool, id).await);
    }
    fetch(pool, id).await
}

/// Pending playable duration per queue, in seconds (§3.8
/// `zukka_queue_fill_seconds{queue}`).
///
/// Approximate lower bound: sums `tracks.duration_ms` of every `pending` item
/// whose `kind = 'track'`, joined through `queue_items.track_id`. Items without
/// a track (`link`, `tape` or announcements) or without a playable duration
/// count `0`. Queues with no playable pending items appear with `0`. The label
/// value is the queue id (stable, unique); ordered by id.
pub async fn fill_seconds(pool: &SqlitePool) -> ApiResult<Vec<(String, f64)>> {
    #[derive(Debug, sqlx::FromRow)]
    struct Row {
        queue_id: String,
        total_ms: i64,
    }

    let rows: Vec<Row> = sqlx::query_as(
        "SELECT q.id AS queue_id, \
                COALESCE(SUM(t.duration_ms), 0) AS total_ms \
         FROM queues q \
         LEFT JOIN queue_items qi \
           ON qi.queue_id = q.id AND qi.state = 'pending' \
              AND qi.kind = 'track' AND qi.track_id IS NOT NULL \
         LEFT JOIN tracks t ON t.id = qi.track_id \
         GROUP BY q.id ORDER BY q.id",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| (row.queue_id, row.total_ms.max(0) as f64 / 1000.0))
        .collect())
}

/// Distinguishes a stale version (`409`) from a missing row (`404`).
async fn stale_or_missing(pool: &SqlitePool, id: &str) -> ApiError {
    let exists: Option<i64> = sqlx::query_scalar("SELECT version FROM queues WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);
    match exists {
        Some(_) => ApiError::conflict("version_conflict", "queue was modified by another client"),
        None => ApiError::not_found("queue", id),
    }
}

/// Deletes a queue; `queue_items` cascade via the DB FK.
pub async fn delete(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    // NOTE: `broadcasts.queue_id` is `ON DELETE SET NULL`, so a queue referenced
    // by a playlist broadcast can be deleted without a FK error. An explicit
    // "in use" gate (`409 queue_in_use`) is out of scope for #10.
    let res = sqlx::query("DELETE FROM queues WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("queue", id));
    }
    Ok(())
}

/// Reorders all items of a queue inside one transaction.
pub async fn reorder(pool: &SqlitePool, id: &str, req: &ReorderRequest) -> ApiResult<QueueDto> {
    let version = req
        .version
        .ok_or_else(|| ApiError::validation("version", "version is required"))?;

    let mut tx = pool.begin().await?;

    let current: Option<i64> = sqlx::query_scalar("SELECT version FROM queues WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(current) = current else {
        return Err(ApiError::not_found("queue", id));
    };
    if current != version {
        return Err(ApiError::conflict(
            "version_conflict",
            "queue was modified by another client",
        ));
    }

    let current_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM queue_items WHERE queue_id = ? ORDER BY position, id")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;

    let mut requested = req.item_ids.clone();
    let mut sorted_current = current_ids.clone();
    requested.sort();
    sorted_current.sort();
    let has_duplicates = {
        let mut seen = requested.clone();
        seen.dedup();
        seen.len() != requested.len()
    };
    if has_duplicates || requested != sorted_current {
        return Err(ApiError::validation(
            "item_ids",
            "item_ids must contain exactly the current item ids of this queue",
        ));
    }

    // Offset trick: move every row out of the 0..n-1 range first so the
    // UNIQUE (queue_id, position) constraint cannot collide mid-rewrite.
    const OFFSET: i64 = 1_000_000;
    sqlx::query("UPDATE queue_items SET position = position + ? WHERE queue_id = ?")
        .bind(OFFSET)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    for (index, item_id) in req.item_ids.iter().enumerate() {
        sqlx::query("UPDATE queue_items SET position = ? WHERE id = ? AND queue_id = ?")
            .bind(index as i64)
            .bind(item_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }

    let ts = now_ts();
    sqlx::query("UPDATE queues SET version = version + 1, updated_at = ? WHERE id = ?")
        .bind(&ts)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let dto = fetch_tx(&mut tx, id).await?;
    tx.commit().await?;
    Ok(dto)
}

/// Removes every item from a queue and bumps `queues.version`.
pub async fn clear(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let mut tx = pool.begin().await?;

    let exists: Option<i64> = sqlx::query_scalar("SELECT version FROM queues WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(ApiError::not_found("queue", id));
    }

    sqlx::query("DELETE FROM queue_items WHERE queue_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let ts = now_ts();
    sqlx::query("UPDATE queues SET version = version + 1, updated_at = ? WHERE id = ?")
        .bind(&ts)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}
