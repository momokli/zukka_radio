//! Queue-item store: read/write queries for `queue_items`.

use sqlx::sqlite::Sqlite;
use sqlx::{SqlitePool, Transaction};

use crate::dto::{
    CreateItemRequest, LinkProvider, LinkState, QueueItemDto, QueueItemKind, QueueItemState,
    UpdateItemRequest,
};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
use crate::store::now_ts;
use crate::uuid::new_id;

/// Row shape of `queue_items`, mapped straight onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct ItemRow {
    id: String,
    queue_id: String,
    position: i64,
    kind: QueueItemKind,
    track_id: Option<String>,
    tape_id: Option<String>,
    link_url: Option<String>,
    link_provider: Option<LinkProvider>,
    link_state: Option<LinkState>,
    title_override: Option<String>,
    state: QueueItemState,
    played_at: Option<String>,
    created_at: String,
}

impl From<ItemRow> for QueueItemDto {
    fn from(row: ItemRow) -> Self {
        QueueItemDto {
            id: row.id,
            queue_id: row.queue_id,
            position: row.position,
            kind: row.kind,
            track_id: row.track_id,
            tape_id: row.tape_id,
            link_url: row.link_url,
            link_provider: row.link_provider,
            link_state: row.link_state,
            title_override: row.title_override,
            state: row.state,
            played_at: row.played_at,
            created_at: row.created_at,
        }
    }
}

const SELECT: &str = "SELECT id, queue_id, position, kind, track_id, tape_id, link_url, \
     link_provider, link_state, title_override, state, played_at, created_at FROM queue_items";

async fn queue_exists(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queues WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("queue", id));
    }
    Ok(())
}

/// Bumps `queues.version` after an item mutation (content == state, O-2).
async fn bump_queue(tx: &mut Transaction<'_, Sqlite>, queue_id: &str) -> ApiResult<()> {
    let ts = now_ts();
    sqlx::query("UPDATE queues SET version = version + 1, updated_at = ? WHERE id = ?")
        .bind(&ts)
        .bind(queue_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Lists a queue's items ordered by `position, id`.
pub async fn list(pool: &SqlitePool, queue_id: &str) -> ApiResult<Vec<QueueItemDto>> {
    queue_exists(pool, queue_id).await?;
    let sql = format!("{SELECT} WHERE queue_id = ? ORDER BY position, id");
    let rows: Vec<ItemRow> = sqlx::query_as(&sql).bind(queue_id).fetch_all(pool).await?;
    Ok(rows.into_iter().map(QueueItemDto::from).collect())
}

/// Adds an item; validates the exactly-one-content matrix and `position`.
pub async fn add(
    pool: &SqlitePool,
    queue_id: &str,
    req: &CreateItemRequest,
) -> ApiResult<QueueItemDto> {
    queue_exists(pool, queue_id).await?;
    let kind = QueueItemKind::parse("kind", &req.kind)?;
    validate_content(kind, req)?;

    if let Some(position) = req.position {
        if position < 0 {
            return Err(ApiError::validation("position", "position must be >= 0"));
        }
    }

    // Validate referenced content exists (FK → 422 with a precise field).
    match kind {
        QueueItemKind::Track => {
            let id = req.track_id.as_deref().unwrap_or_default();
            ensure_row(pool, "tracks", id, "track_id").await?;
        }
        QueueItemKind::Tape => {
            let id = req.tape_id.as_deref().unwrap_or_default();
            ensure_row(pool, "tapes", id, "tape_id").await?;
        }
        QueueItemKind::Link => {}
    }

    let link_provider = match &req.link_provider {
        Some(raw) => Some(LinkProvider::parse("link_provider", raw)?),
        None => None,
    };
    let link_state = (kind == QueueItemKind::Link).then_some(LinkState::Pending);

    let mut tx = pool.begin().await?;

    let position = match req.position {
        Some(position) => position,
        None => {
            let max: Option<i64> =
                sqlx::query_scalar("SELECT MAX(position) FROM queue_items WHERE queue_id = ?")
                    .bind(queue_id)
                    .fetch_one(&mut *tx)
                    .await?;
            max.map_or(0, |m| m + 1)
        }
    };

    let id = new_id();
    let ts = now_ts();
    let res = sqlx::query(
        "INSERT INTO queue_items \
         (id, queue_id, position, kind, track_id, tape_id, link_url, link_provider, link_state, \
          title_override, state, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(&id)
    .bind(queue_id)
    .bind(position)
    .bind(kind.as_str())
    .bind(req.track_id.as_deref())
    .bind(req.tape_id.as_deref())
    .bind(req.link_url.as_deref())
    .bind(link_provider.map(LinkProvider::as_str))
    .bind(link_state.map(link_state_str))
    .bind(req.title_override.as_deref())
    .bind(&ts)
    .execute(&mut *tx)
    .await;

    if let Err(err) = res {
        if let Some(msg) = unique_violation(&err) {
            if msg.contains("queue_items") {
                return Err(ApiError::conflict(
                    "duplicate_position",
                    "another item already occupies this position",
                ));
            }
        }
        if is_foreign_key_violation(&err) {
            return Err(ApiError::validation(
                "content",
                "referenced track or tape does not exist",
            ));
        }
        return Err(err.into());
    }

    bump_queue(&mut tx, queue_id).await?;
    let item = fetch_tx(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(item)
}

/// Updates mutable item fields (`title_override`, `state`, `position`).
pub async fn update(
    pool: &SqlitePool,
    queue_id: &str,
    item_id: &str,
    req: &UpdateItemRequest,
) -> ApiResult<QueueItemDto> {
    let current = fetch_optional(pool, queue_id, item_id)
        .await?
        .ok_or_else(|| ApiError::not_found("queue item", item_id))?;

    let state = match &req.state {
        Some(raw) => Some(QueueItemState::parse("state", raw)?),
        None => None,
    };

    if let Some(position) = req.position {
        if position < 0 {
            return Err(ApiError::validation("position", "position must be >= 0"));
        }
        if position != current.position {
            let taken: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_items WHERE queue_id = ? AND position = ? AND id != ?",
            )
            .bind(queue_id)
            .bind(position)
            .bind(item_id)
            .fetch_one(pool)
            .await?;
            if taken > 0 {
                return Err(ApiError::validation(
                    "position",
                    "another item already occupies this position",
                ));
            }
        }
    }

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE queue_items SET \
         title_override = COALESCE(?, title_override), \
         state = COALESCE(?, state), \
         position = COALESCE(?, position) \
         WHERE id = ? AND queue_id = ?",
    )
    .bind(req.title_override.as_deref())
    .bind(state.map(state_str))
    .bind(req.position)
    .bind(item_id)
    .bind(queue_id)
    .execute(&mut *tx)
    .await?;

    bump_queue(&mut tx, queue_id).await?;
    let item = fetch_tx(&mut tx, item_id).await?;
    tx.commit().await?;
    Ok(item)
}

/// Deletes an item; a gap in `position` is allowed (no reindex).
pub async fn delete(pool: &SqlitePool, queue_id: &str, item_id: &str) -> ApiResult<()> {
    let mut tx = pool.begin().await?;
    let res = sqlx::query("DELETE FROM queue_items WHERE id = ? AND queue_id = ?")
        .bind(item_id)
        .bind(queue_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("queue item", item_id));
    }
    bump_queue(&mut tx, queue_id).await?;
    tx.commit().await?;
    Ok(())
}

async fn fetch_optional(
    pool: &SqlitePool,
    queue_id: &str,
    item_id: &str,
) -> ApiResult<Option<QueueItemDto>> {
    let sql = format!("{SELECT} WHERE id = ? AND queue_id = ?");
    let row: Option<ItemRow> = sqlx::query_as(&sql)
        .bind(item_id)
        .bind(queue_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(QueueItemDto::from))
}

async fn fetch_tx(tx: &mut Transaction<'_, Sqlite>, item_id: &str) -> ApiResult<QueueItemDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<ItemRow> = sqlx::query_as(&sql)
        .bind(item_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(QueueItemDto::from)
        .ok_or_else(|| ApiError::not_found("queue item", item_id))
}

async fn ensure_row(
    pool: &SqlitePool,
    table: &str,
    id: &str,
    field: &'static str,
) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE id = ?"))
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::validation(field, "referenced row does not exist"));
    }
    Ok(())
}

/// Enforces the "exactly one content field" matrix from §3.3 in Rust.
fn validate_content(kind: QueueItemKind, req: &CreateItemRequest) -> ApiResult<()> {
    let has_track = req.track_id.is_some();
    let has_tape = req.tape_id.is_some();
    let has_link = req.link_url.is_some();
    match kind {
        QueueItemKind::Track => {
            if !has_track || has_tape || has_link {
                return Err(ApiError::validation(
                    "kind",
                    "kind 'track' requires track_id and no tape_id/link_url",
                ));
            }
        }
        QueueItemKind::Tape => {
            if !has_tape || has_track || has_link {
                return Err(ApiError::validation(
                    "kind",
                    "kind 'tape' requires tape_id and no track_id/link_url",
                ));
            }
        }
        QueueItemKind::Link => {
            if !has_link || has_track || has_tape {
                return Err(ApiError::validation(
                    "kind",
                    "kind 'link' requires link_url and no track_id/tape_id",
                ));
            }
        }
    }
    Ok(())
}

fn state_str(state: QueueItemState) -> &'static str {
    match state {
        QueueItemState::Pending => "pending",
        QueueItemState::Playing => "playing",
        QueueItemState::Played => "played",
        QueueItemState::Skipped => "skipped",
    }
}

fn link_state_str(state: LinkState) -> &'static str {
    match state {
        LinkState::Pending => "pending",
        LinkState::Ready => "ready",
        LinkState::Failed => "failed",
        LinkState::Unplayable => "unplayable",
    }
}
