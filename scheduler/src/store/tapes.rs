//! Tape registration for live broadcasts (Issue #108, M9).
//!
//! A `kind='live'` broadcast gets exactly one `tapes` row, created inside the
//! same transaction as the status transition. Recording itself happens in
//! Liquidsoap (`output.file`, see `liquidsoap/lib/tape.liq`); this module only
//! persists the tape lifecycle the scheduler owns.
//!
//! Schema notes (deployed `scheduler/migrations/0001_init.sql`):
//!
//! - `tapes.ended_at` is `NOT NULL` with `CHECK (ended_at >= started_at)`. A
//!   freshly started tape therefore uses a placeholder `ended_at = started_at`
//!   and is finalized at broadcast end.
//! - A `live` broadcast must not carry a `tape_id` (kind/content matrix); the
//!   link is the other way round, via `tapes.broadcast_id`.
//! - `status` stays `pending` and `duration_ms` stays `NULL`: the probe/peaks
//!   `tape_jobs` and the upload to the music API are out of scope for #108
//!   (see `docs/liquidsoap.md` §3.4).

use chrono::{DateTime, Utc};
use sqlx::{SqlitePool, sqlite::SqliteTransaction};

use crate::dto::{ListTapesParams, TapeDto};
use crate::error::{ApiError, ApiResult};
use crate::migrate::slug::slug_from_key;
use crate::store::now_ts;
use crate::store::tape_jobs::UPLOAD_KIND;
use crate::time::fmt_storage_time;
use crate::uuid::new_id;

/// Inserts a `source='live'`, `status='pending'` tape for a broadcast start.
///
/// Returns the new tape `id` (UUIDv7). `slug` is the stable 8-char Crockford
/// slug derived from the id. `ended_at` is a placeholder equal to `started_at`
/// until [`finalize_live_tape`] stamps the real end.
pub async fn register_live_tape(
    tx: &mut SqliteTransaction<'_>,
    broadcast_id: &str,
    caster_id: &str,
    show_id: Option<&str>,
    title: &str,
    started_at: DateTime<Utc>,
) -> ApiResult<String> {
    let id = new_id();
    let slug = slug_from_key(&id);
    let ts = fmt_storage_time(started_at);
    let now = now_ts();
    sqlx::query(
        "INSERT INTO tapes \
         (id, slug, caster_id, show_id, broadcast_id, title, source, started_at, ended_at, \
          status, visible, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'live', ?, ?, 'pending', 0, ?, ?)",
    )
    .bind(&id)
    .bind(&slug)
    .bind(caster_id)
    .bind(show_id)
    .bind(broadcast_id)
    .bind(title)
    .bind(&ts)
    .bind(&ts)
    .bind(&now)
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Finalizes the newest pending live tape of a broadcast end.
///
/// Stamps `ended_at` and `updated_at`; `status` stays `pending` and
/// `duration_ms` stays `NULL` (ffprobe is a later stage, out of scope for
/// #108). In the same transaction it enqueues the `kind='upload'` ingest job
/// (`state='pending'`, `attempts=0`) so the `tape_ingest` poll loop (§3.6) picks
/// the finished recording up. The insert is idempotent
/// (`ON CONFLICT (tape_id, kind) DO NOTHING`), so a repeated finalize cannot
/// double-enqueue. Returns the tape `id`, or `None` when no pending tape exists
/// (e.g. the start was never registered) — a warning, never an error.
pub async fn finalize_live_tape(
    tx: &mut SqliteTransaction<'_>,
    broadcast_id: &str,
    ended_at: DateTime<Utc>,
) -> ApiResult<Option<String>> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM tapes \
         WHERE broadcast_id = ? AND source = 'live' AND status = 'pending' \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(broadcast_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(id) = id else {
        return Ok(None);
    };

    let ended = fmt_storage_time(ended_at);
    let now = now_ts();
    // Clamp to the tape's own `started_at`: the schema enforces
    // `CHECK (ended_at >= started_at)` and the caller may pass a `now` that
    // trails the tape start (clock step, cancel shortly after start). The
    // canonical UTC storage form makes the text `max()` chronological.
    sqlx::query("UPDATE tapes SET ended_at = max(?, started_at), updated_at = ? WHERE id = ?")
        .bind(&ended)
        .bind(&now)
        .bind(&id)
        .execute(&mut **tx)
        .await?;

    // Enqueue the ingest job in the same transaction: without it the tape
    // would never be uploaded (no other writer creates a `kind='upload'` row
    // at broadcast end). `ON CONFLICT DO NOTHING` keeps a repeated finalize
    // (or a pre-existing job) from duplicating the row.
    sqlx::query(
        "INSERT INTO tape_jobs (id, tape_id, kind, state, attempts, created_at, updated_at) \
         VALUES (?, ?, ?, 'pending', 0, ?, ?) ON CONFLICT (tape_id, kind) DO NOTHING",
    )
    .bind(new_id())
    .bind(&id)
    .bind(UPLOAD_KIND)
    .bind(&now)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    Ok(Some(id))
}

/// Row shape of `tapes`, mapped onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct TapeRow {
    id: String,
    slug: String,
    caster_id: String,
    show_id: Option<String>,
    broadcast_id: Option<String>,
    title: String,
    description: Option<String>,
    started_at: String,
    ended_at: String,
    duration_ms: Option<i64>,
    source: String,
    music_api_id: Option<String>,
    storage_key: Option<String>,
    format: Option<String>,
    mime: Option<String>,
    size_bytes: Option<i64>,
    etag: Option<String>,
    status: String,
    visible: bool,
    published_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TapeRow {
    fn into_dto(self) -> TapeDto {
        TapeDto {
            id: self.id,
            slug: self.slug,
            caster_id: self.caster_id,
            show_id: self.show_id,
            broadcast_id: self.broadcast_id,
            title: self.title,
            description: self.description,
            started_at: self.started_at,
            ended_at: self.ended_at,
            duration_ms: self.duration_ms,
            source: self.source,
            music_api_id: self.music_api_id,
            storage_key: self.storage_key,
            format: self.format,
            mime: self.mime,
            size_bytes: self.size_bytes,
            etag: self.etag,
            status: self.status,
            visible: self.visible,
            published_at: self.published_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

const SELECT: &str = "SELECT id, slug, caster_id, show_id, broadcast_id, title, description, \
     started_at, ended_at, duration_ms, source, music_api_id, storage_key, format, mime, size_bytes, etag, \
     status, visible, published_at, created_at, updated_at FROM tapes";

/// The visibility scope predicate: visible=1 (public) OR caster_id = viewer.
const SCOPE: &str = "(visible = 1 OR caster_id = ?)";

/// Lists/searches tapes with the viewer scope.
///
/// Viewer-Scope: `WHERE (visible=1 OR caster_id=viewer)` — analog `tracks::search`.
/// The `tapes` table has no `visibility` column, only `visible` (0/1) and `source` (`live|upload`).
/// Filter `visibility` parameter is accepted but maps to `visible` boolean.
pub async fn search(
    pool: &SqlitePool,
    params: &ListTapesParams,
    viewer: Option<&str>,
) -> ApiResult<Vec<TapeDto>> {
    let like = params.q.as_ref().map(|q| format!("%{}%", q.trim()));
    let sql = format!(
        "{SELECT} \
         WHERE (? IS NULL OR title LIKE ?) \
         AND (? IS NULL OR visible = ?) \
         AND (? IS NULL OR caster_id = ?) \
         AND (? IS NULL OR source = ?) \
         AND (? IS NULL OR status = ?) \
         AND {SCOPE} \
         ORDER BY created_at DESC, id LIMIT ? OFFSET ?"
    );
    let rows: Vec<TapeRow> = sqlx::query_as(&sql)
        .bind(params.q.as_deref())
        .bind(like.as_deref())
        .bind(params.visible)
        .bind(params.visible.map(|v| if v { 1i64 } else { 0i64 }))
        .bind(params.owner_caster_id.as_deref())
        .bind(params.owner_caster_id.as_deref())
        .bind(params.source.as_deref())
        .bind(params.source.as_deref())
        .bind(params.status.as_deref())
        .bind(params.status.as_deref())
        .bind(viewer)
        .bind(params.limit.unwrap_or(50).min(200))
        .bind(params.offset.unwrap_or(0).max(0))
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TapeRow::into_dto).collect())
}

/// Fetches one tape within the viewer's visibility scope, else `404`.
pub async fn get(pool: &SqlitePool, id: &str, viewer: Option<&str>) -> ApiResult<TapeDto> {
    let sql = format!("{SELECT} WHERE id = ? AND {SCOPE}");
    let row: Option<TapeRow> = sqlx::query_as(&sql)
        .bind(id)
        .bind(viewer)
        .fetch_optional(pool)
        .await?;
    match row {
        Some(row) => Ok(row.into_dto()),
        None => Err(ApiError::not_found("tape", id)),
    }
}
