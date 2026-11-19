//! Upload-job store (Issue #18): restart-safe job tracking for portal uploads.
//!
//! The scheduler is the single writer of `upload_jobs` (§4.2, P2). The portal
//! reaches these functions only through `POST`/`PATCH /internal/upload-jobs`.
//! States follow `pending → running → done|failed`; a `running`/`failed` job
//! may be requeued to `pending` (recovery, US-10). `attempts` counts the number
//! of times a job entered `running`.

use sqlx::SqlitePool;

use crate::dto::{CreateUploadJobRequest, PatchUploadJobRequest, UploadJobDto};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;
use crate::uuid::new_id;

/// The four legal states of an upload job (`upload_jobs.state` CHECK).
pub const STATES: [&str; 4] = ["pending", "running", "done", "failed"];

#[derive(Debug, sqlx::FromRow)]
struct UploadJobRow {
    id: String,
    caster_id: Option<String>,
    filename: String,
    size_bytes: Option<i64>,
    sha256: Option<String>,
    state: String,
    track_id: Option<String>,
    error: Option<String>,
    attempts: i64,
    created_at: String,
    updated_at: String,
}

impl UploadJobRow {
    fn into_dto(self) -> UploadJobDto {
        UploadJobDto {
            id: self.id,
            caster_id: self.caster_id,
            filename: self.filename,
            size_bytes: self.size_bytes,
            sha256: self.sha256,
            state: self.state,
            track_id: self.track_id,
            error: self.error,
            attempts: self.attempts,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

const SELECT: &str = "SELECT id, caster_id, filename, size_bytes, sha256, state, track_id, \
     error, attempts, created_at, updated_at FROM upload_jobs";

/// Allowed state transitions. `from == to` is an idempotent update (e.g.
/// refreshing `sha256` while still `running`). Everything else is a `409`.
pub fn transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        ("pending", "running")
            | ("pending", "failed")
            | ("running", "done")
            | ("running", "failed")
            // Recovery (US-10): a stale running job is requeued or failed.
            | ("running", "pending")
            | ("failed", "pending")
            | ("failed", "running")
    )
}

/// Creates a job in state `pending` with a fresh UUIDv7 and returns it.
pub async fn create(pool: &SqlitePool, req: &CreateUploadJobRequest) -> ApiResult<UploadJobDto> {
    let id = new_id();
    let now = now_ts();
    sqlx::query(
        "INSERT INTO upload_jobs (id, caster_id, filename, size_bytes, sha256, state, \
         attempts, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'pending', 0, ?, ?)",
    )
    .bind(&id)
    .bind(&req.caster_id)
    .bind(&req.filename)
    .bind(req.size_bytes)
    .bind(&req.sha256)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    get(pool, &id).await
}

/// Fetches one job or `404`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<UploadJobDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    sqlx::query_as::<_, UploadJobRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(UploadJobRow::into_dto)
        .ok_or_else(|| ApiError::not_found("upload_job", id))
}

/// Lists jobs, newest first (bounded by `limit`).
pub async fn list(pool: &SqlitePool, limit: i64) -> ApiResult<Vec<UploadJobDto>> {
    let sql = format!("{SELECT} ORDER BY created_at DESC, id DESC LIMIT ?");
    let rows = sqlx::query_as::<_, UploadJobRow>(&sql)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(UploadJobRow::into_dto).collect())
}

/// Finds a completed job for `sha256` that produced a track — the dedupe
/// fallback lookup (D6/D8). Returns the most recent match, if any.
///
/// The portal's upload pipeline dedupes primarily against the Music API
/// (`GET /tracks/by-hash/{sha256}`); this store lookup is the scheduler-side
/// fallback and is also the query the tapes-worker can reuse. Kept public so it
/// is not dead code.
pub async fn find_by_hash(pool: &SqlitePool, sha256: &str) -> ApiResult<Option<UploadJobDto>> {
    let sql = format!(
        "{SELECT} WHERE sha256 = ? AND state = 'done' AND track_id IS NOT NULL \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    );
    let row = sqlx::query_as::<_, UploadJobRow>(&sql)
        .bind(sha256)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(UploadJobRow::into_dto))
}

/// Applies a state patch after validating the transition. `sha256`/`track_id`
/// are only overwritten when present; `error` is replaced (a `None` clears it).
/// Entering `running` increments `attempts`.
pub async fn patch_state(
    pool: &SqlitePool,
    id: &str,
    patch: &PatchUploadJobRequest,
) -> ApiResult<UploadJobDto> {
    let current = sqlx::query_as::<_, UploadJobRow>(&format!("{SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("upload_job", id))?;

    if !transition_allowed(&current.state, &patch.state) {
        return Err(ApiError::conflict(
            "invalid_state_transition",
            format!(
                "upload job {id} cannot move from '{}' to '{}'",
                current.state, patch.state
            ),
        ));
    }

    let increment = i64::from(patch.state == "running");
    sqlx::query(
        "UPDATE upload_jobs SET state = ?, sha256 = COALESCE(?, sha256), \
         track_id = COALESCE(?, track_id), error = ?, attempts = attempts + ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&patch.state)
    .bind(&patch.sha256)
    .bind(&patch.track_id)
    .bind(&patch.error)
    .bind(increment)
    .bind(now_ts())
    .bind(id)
    .execute(pool)
    .await?;

    get(pool, id).await
}

/// Lists `running` jobs whose `updated_at` is older than `older_than` (an
/// RFC3339 UTC ms string, lexicographically comparable per §3.1). Used by the
/// startup recovery task (US-10).
pub async fn list_stale_running(
    pool: &SqlitePool,
    older_than: &str,
) -> ApiResult<Vec<UploadJobDto>> {
    let sql =
        format!("{SELECT} WHERE state = 'running' AND updated_at < ? ORDER BY updated_at ASC");
    let rows = sqlx::query_as::<_, UploadJobRow>(&sql)
        .bind(older_than)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(UploadJobRow::into_dto).collect())
}
