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
use sqlx::sqlite::SqliteTransaction;

use crate::error::ApiResult;
use crate::migrate::slug::slug_from_key;
use crate::store::now_ts;
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
/// #108). Returns the tape `id`, or `None` when no pending tape exists (e.g.
/// the start was never registered) — a warning, never an error.
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
    Ok(Some(id))
}
