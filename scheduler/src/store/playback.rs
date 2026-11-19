//! Playback store: layer resolution, item projection and idempotent state
//! transitions (Issue #13).
//!
//! ## Why exactly one queue is "on air" (§2.6)
//!
//! A queue is a persistent *ordered list*, not a parallel player: exactly one
//! queue is on air at a time (layer 60). The scheduler decides which one — the
//! `slot`-queue of the running `playlist` broadcast first, then
//! `settings['queue.active_id']`, otherwise layer 60 is absent. The `show`
//! layer (80) returns the same broadcast content at a higher priority so a
//! running `recording`/`playlist` broadcast is served even without
//! `queue.active_id`, while `live` (100) stays untouched.
//!
//! `queue_items.kind = 'link'` is not directly playable (its Music-API URL only
//! exists after resolution, O7), so `link` items are **skipped** during
//! selection in #13; resolution is a follow-up issue.

use sqlx::SqlitePool;

use crate::dto::{
    PlaybackEndedRequest, PlaybackErrorRequest, PlaybackItemDto, PlaybackKind, PlaybackLayer,
    PlaybackStartedRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;

/// Base URL of the Music API (C46/C85), overridable via `MUSIC_API_BASE_URL`.
pub(crate) fn music_api_base() -> String {
    std::env::var("MUSIC_API_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:5001".to_string())
}

/// Projection row: a queue item joined with its media object.
#[derive(Debug, sqlx::FromRow)]
struct ItemRow {
    queue_item_id: String,
    qkind: String,
    track_id: Option<String>,
    tape_id: Option<String>,
    title_override: Option<String>,
    track_title: Option<String>,
    track_artist: Option<String>,
    track_duration_ms: Option<i64>,
    track_music_api_id: Option<String>,
    tape_title: Option<String>,
    tape_duration_ms: Option<i64>,
    tape_music_api_id: Option<String>,
}

const ITEM_SELECT: &str = "SELECT qi.id AS queue_item_id, qi.kind AS qkind, qi.track_id, qi.tape_id, \
     qi.title_override, t.title AS track_title, t.artist AS track_artist, \
     t.duration_ms AS track_duration_ms, t.music_api_id AS track_music_api_id, \
     tp.title AS tape_title, tp.duration_ms AS tape_duration_ms, \
     tp.music_api_id AS tape_music_api_id \
     FROM queue_items qi \
     LEFT JOIN tracks t ON t.id = qi.track_id \
     LEFT JOIN tapes tp ON tp.id = qi.tape_id";

impl ItemRow {
    /// Projects a queue item onto [`PlaybackItemDto`] (C46 `uri`).
    fn into_dto(self) -> ApiResult<PlaybackItemDto> {
        let base = music_api_base();
        match self.qkind.as_str() {
            "track" => Ok(PlaybackItemDto {
                queue_item_id: Some(self.queue_item_id),
                kind: PlaybackKind::Track,
                track_id: self.track_id,
                tape_id: None,
                title: self.title_override.or(self.track_title).unwrap_or_default(),
                artist: self.track_artist,
                duration_ms: self.track_duration_ms.unwrap_or(0),
                uri: format!(
                    "{base}/stream/tracks/{}",
                    self.track_music_api_id.unwrap_or_default()
                ),
                cue_in_ms: None,
                cue_out_ms: None,
            }),
            "tape" => Ok(PlaybackItemDto {
                queue_item_id: Some(self.queue_item_id),
                kind: PlaybackKind::Tape,
                track_id: None,
                tape_id: self.tape_id,
                title: self.title_override.or(self.tape_title).unwrap_or_default(),
                artist: None,
                duration_ms: self.tape_duration_ms.unwrap_or(0),
                uri: format!(
                    "{base}/stream/tapes/{}",
                    self.tape_music_api_id.unwrap_or_default()
                ),
                cue_in_ms: None,
                cue_out_ms: None,
            }),
            other => Err(ApiError::validation(
                "kind",
                format!("queue item kind '{other}' is not playable"),
            )),
        }
    }
}

/// Live broadcast shape used by `show` / `queue` resolution (§2.2).
#[derive(Debug, sqlx::FromRow)]
struct LiveBroadcast {
    kind: String,
    tape_id: Option<String>,
    queue_id: Option<String>,
}

async fn live_broadcast(pool: &SqlitePool) -> ApiResult<Option<LiveBroadcast>> {
    let row: Option<LiveBroadcast> = sqlx::query_as(
        "SELECT kind, tape_id, queue_id FROM broadcasts \
         WHERE status = 'live' ORDER BY starts_at, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The queue currently on air (layer 60) — §2.2 priority chain.
async fn active_queue_id(pool: &SqlitePool) -> ApiResult<Option<String>> {
    // 1. the `slot`-queue of the running playlist broadcast.
    let from_broadcast: Option<String> = sqlx::query_scalar(
        "SELECT queue_id FROM broadcasts \
         WHERE status = 'live' AND kind = 'playlist' AND queue_id IS NOT NULL \
         ORDER BY starts_at, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    if let Some(id) = from_broadcast {
        return Ok(Some(id));
    }
    // 2. fallback: settings['queue.active_id'] when it points at a real queue.
    let active: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'queue.active_id'")
            .fetch_optional(pool)
            .await?;
    let Some(active) = active else {
        return Ok(None);
    };
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queues WHERE id = ?")
        .bind(&active)
        .fetch_one(pool)
        .await?;
    Ok((exists > 0).then_some(active))
}

/// Smallest-position pending `track`/`tape` of `queue_id` (§2.2/§2.4).
async fn next_pending_item(
    pool: &SqlitePool,
    queue_id: &str,
) -> ApiResult<Option<PlaybackItemDto>> {
    let sql = format!(
        "{ITEM_SELECT} WHERE qi.queue_id = ? AND qi.state = 'pending' \
         AND qi.kind IN ('track', 'tape') ORDER BY qi.position, qi.id LIMIT 1"
    );
    let row: Option<ItemRow> = sqlx::query_as(&sql)
        .bind(queue_id)
        .fetch_optional(pool)
        .await?;
    row.map(ItemRow::into_dto).transpose()
}

/// A tape not tied to a queue (`show`-`recording`), `queue_item_id = null`.
async fn tape_item(pool: &SqlitePool, tape_id: Option<&str>) -> ApiResult<Option<PlaybackItemDto>> {
    let Some(tape_id) = tape_id else {
        return Ok(None);
    };
    #[derive(sqlx::FromRow)]
    struct TapeRow {
        id: String,
        title: Option<String>,
        duration_ms: Option<i64>,
        music_api_id: Option<String>,
    }
    let row: Option<TapeRow> =
        sqlx::query_as("SELECT id, title, duration_ms, music_api_id FROM tapes WHERE id = ?")
            .bind(tape_id)
            .fetch_optional(pool)
            .await?;
    let base = music_api_base();
    Ok(row.map(|row| PlaybackItemDto {
        queue_item_id: None,
        kind: PlaybackKind::Tape,
        track_id: None,
        tape_id: Some(row.id),
        title: row.title.unwrap_or_default(),
        artist: None,
        duration_ms: row.duration_ms.unwrap_or(0),
        uri: format!(
            "{base}/stream/tapes/{}",
            row.music_api_id.unwrap_or_default()
        ),
        cue_in_ms: None,
        cue_out_ms: None,
    }))
}

/// Resolves `GET /playback/next` for `layer` (§2.2).
pub async fn next(pool: &SqlitePool, layer: PlaybackLayer) -> ApiResult<Option<PlaybackItemDto>> {
    let queue_id = match layer {
        PlaybackLayer::Queue => active_queue_id(pool).await?,
        PlaybackLayer::Show => match live_broadcast(pool).await? {
            None => return Ok(None),
            Some(live) => match live.kind.as_str() {
                "recording" => return tape_item(pool, live.tape_id.as_deref()).await,
                "playlist" => live.queue_id,
                // `live` (prio 100) sits above `show`; nothing to serve.
                _ => return Ok(None),
            },
        },
    };
    match queue_id {
        Some(id) => next_pending_item(pool, &id).await,
        None => Ok(None),
    }
}

/// Resolves `GET /playback/current` (§2.5).
pub async fn current(pool: &SqlitePool) -> ApiResult<Option<PlaybackItemDto>> {
    // 1. the item in state 'playing'; defensively the smallest position.
    let playing_sql = format!(
        "{ITEM_SELECT} WHERE qi.state = 'playing' AND qi.kind IN ('track', 'tape') \
         ORDER BY qi.position, qi.id LIMIT 1"
    );
    if let Some(row) = sqlx::query_as::<_, ItemRow>(&playing_sql)
        .fetch_optional(pool)
        .await?
    {
        return Ok(Some(row.into_dto()?));
    }
    // 2. otherwise the most recently reported item (MAX(played_at)).
    let played_sql = format!(
        "{ITEM_SELECT} WHERE qi.played_at IS NOT NULL AND qi.kind IN ('track', 'tape') \
         ORDER BY qi.played_at DESC, qi.id DESC LIMIT 1"
    );
    let row: Option<ItemRow> = sqlx::query_as(&played_sql).fetch_optional(pool).await?;
    row.map(ItemRow::into_dto).transpose()
}

/// Requires a non-empty `queue_item_id`, else `422 validation_error`.
fn required_id(field: &'static str, raw: Option<&str>) -> ApiResult<String> {
    match raw {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(ApiError::validation(field, format!("{field} is required"))),
    }
}

async fn ensure_item(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_items WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("queue item", id));
    }
    Ok(())
}

/// `POST /playback/started` — `pending → playing`, idempotent (§2.4).
pub async fn started(pool: &SqlitePool, req: &PlaybackStartedRequest) -> ApiResult<()> {
    let id = required_id("queue_item_id", req.queue_item_id.as_deref())?;
    // Informational `kind`, if present, must still be a valid playable value.
    if let Some(raw) = req.kind.as_deref() {
        PlaybackKind::parse("kind", raw)?;
    }
    ensure_item(pool, &id).await?;
    sqlx::query("UPDATE queue_items SET state = 'playing' WHERE id = ? AND state = 'pending'")
        .bind(&id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `POST /playback/ended` — `pending`/`playing → played`, `played_at` once (§2.4).
pub async fn ended(pool: &SqlitePool, req: &PlaybackEndedRequest) -> ApiResult<()> {
    let id = required_id("queue_item_id", req.queue_item_id.as_deref())?;
    ensure_item(pool, &id).await?;
    let ts = now_ts();
    sqlx::query(
        "UPDATE queue_items SET state = 'played', played_at = COALESCE(played_at, ?) \
         WHERE id = ? AND state IN ('pending', 'playing')",
    )
    .bind(&ts)
    .bind(&id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `POST /playback/error` — `pending`/`playing → skipped`, idempotent (§2.4).
pub async fn error(pool: &SqlitePool, req: &PlaybackErrorRequest) -> ApiResult<()> {
    let id = required_id("queue_item_id", req.queue_item_id.as_deref())?;
    ensure_item(pool, &id).await?;
    sqlx::query(
        "UPDATE queue_items SET state = 'skipped' WHERE id = ? AND state IN ('pending', 'playing')",
    )
    .bind(&id)
    .execute(pool)
    .await?;
    Ok(())
}
