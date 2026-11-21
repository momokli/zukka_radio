//! Tape-job queue for the waveform pipeline (Issue #25, M3).
//!
//! The scheduler is the single writer of `tape_jobs` and `tapes` (P2). The
//! read-only tapes-worker claims jobs and reports progress through
//! `POST /internal/tape-jobs/claim` and `PATCH /internal/tape-jobs/{id}`.
//!
//! `tape_jobs` has **no** `run_after` column (deployed `0001_init.sql`); retry
//! backoff is therefore encoded as `updated_at + backoff(attempts)` inside the
//! claim query. `attempts` is incremented on every claim, so a job that failed
//! `n` times waits `backoff(n)` before it becomes due again.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sqlx::SqlitePool;

use crate::dto::{PatchTapeJobRequest, TapeJobClaimDto, TapeJobDto};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;
use crate::uuid::new_id;

/// Legal `kind` values (`tape_jobs.kind` CHECK).
pub const KINDS: [&str; 2] = ["probe", "peaks"];

/// Ingest job kind (#132). Never claimable by the read-only tapes-worker —
/// only `tape_ingest` claims it directly.
pub const UPLOAD_KIND: &str = "upload";

/// Terminal retry cap for the upload job: after this many attempts the tape is
/// marked `failed` and the staging file is kept for manual recovery.
pub const MAX_UPLOAD_ATTEMPTS: i64 = 8;

/// Legal `state` values (`tape_jobs.state` CHECK).
pub const STATES: [&str; 4] = ["pending", "running", "done", "failed"];

/// Exponential retry backoff: `5 * 2^(attempts-1)` seconds, capped at 300 s.
///
/// `attempts == 0` (never claimed) is immediately due.
pub fn backoff_secs(attempts: i64) -> i64 {
    if attempts <= 0 {
        return 0;
    }
    let shift = (attempts - 1).min(6) as u32;
    (5i64 << shift).min(300)
}

/// Allowed state transitions. `from == to` is an idempotent patch; a `done`
/// job is terminal (`409` for anything else). A failed job may be retried.
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
            | ("running", "pending")
            | ("failed", "pending")
            | ("failed", "running")
    )
}

#[derive(Debug, sqlx::FromRow)]
struct TapeJobRow {
    id: String,
    tape_id: String,
    kind: String,
    state: String,
    error: Option<String>,
    attempts: i64,
    created_at: String,
    updated_at: String,
}

impl TapeJobRow {
    fn into_dto(self) -> TapeJobDto {
        TapeJobDto {
            id: self.id,
            tape_id: self.tape_id,
            kind: self.kind,
            state: self.state,
            error: self.error,
            attempts: self.attempts,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ClaimCandidate {
    id: String,
    tape_id: String,
    kind: String,
    attempts: i64,
    updated_at: String,
    slug: String,
    storage_key: Option<String>,
    source: String,
}

const JOB_SELECT: &str = "SELECT id, tape_id, kind, state, error, attempts, created_at, \
     updated_at FROM tape_jobs";

fn fmt_ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// True when a pending job's backoff window has elapsed.
fn is_due(attempts: i64, updated_at: &str, now: DateTime<Utc>) -> bool {
    if attempts <= 0 {
        return true;
    }
    match parse_ts(updated_at) {
        Some(then) => then + Duration::seconds(backoff_secs(attempts)) <= now,
        // An unparseable timestamp must never park a job forever.
        None => true,
    }
}

/// Atomically claims up to `limit` due jobs of the requested `kinds`.
///
/// Inside one transaction: (a) expired `running` leases are requeued to
/// `pending`, (b) due `pending` jobs are selected with their tape fields,
/// (c) each selected job moves to `running` with `attempts + 1`. Parallel
/// claims therefore return disjoint sets.
pub async fn claim(
    pool: &SqlitePool,
    kinds: &[String],
    limit: i64,
    lease_secs: i64,
) -> ApiResult<Vec<TapeJobClaimDto>> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }

    let now = Utc::now();
    let now_str = fmt_ts(now);
    let lease_cutoff = fmt_ts(now - Duration::seconds(lease_secs.max(0)));

    let mut tx = pool.begin().await?;

    // (a) lease recovery: a crashed worker's running job becomes pending again.
    // The timestamp is the lease cutoff, not "now", so the per-job retry
    // backoff does not park a recovered job for an extra window.
    sqlx::query(
        "UPDATE tape_jobs SET state = 'pending', updated_at = ? \
         WHERE state = 'running' AND updated_at <= ?",
    )
    .bind(&lease_cutoff)
    .bind(&lease_cutoff)
    .execute(&mut *tx)
    .await?;

    // (b) candidate selection (kind-scoped). Backoff is evaluated per row in
    // Rust because each job has its own `attempts`-derived threshold.
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT j.id, j.tape_id, j.kind, j.attempts, j.updated_at, t.slug, \
         t.storage_key, t.source FROM tape_jobs j \
         JOIN tapes t ON t.id = j.tape_id \
         WHERE j.state = 'pending' AND j.kind IN (",
    );
    let mut sep = qb.separated(", ");
    for kind in kinds {
        sep.push_bind(kind.clone());
    }
    sep.push_unseparated(") ORDER BY j.created_at ASC, j.id ASC LIMIT ");
    qb.push_bind((limit.max(1) * 20).max(20));

    let candidates: Vec<ClaimCandidate> = qb.build_query_as().fetch_all(&mut *tx).await?;

    let mut claimed = Vec::new();
    for candidate in candidates {
        if claimed.len() as i64 >= limit {
            break;
        }
        if !is_due(candidate.attempts, &candidate.updated_at, now) {
            continue;
        }
        let next_attempts = candidate.attempts + 1;
        let result = sqlx::query(
            "UPDATE tape_jobs SET state = 'running', attempts = ?, updated_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(next_attempts)
        .bind(&now_str)
        .bind(&candidate.id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            claimed.push(TapeJobClaimDto {
                id: candidate.id,
                tape_id: candidate.tape_id,
                kind: candidate.kind,
                attempts: next_attempts,
                slug: candidate.slug,
                storage_key: candidate.storage_key,
                source: candidate.source,
            });
        }
    }

    tx.commit().await?;
    Ok(claimed)
}

/// Applies a worker patch (state + probe metadata) and its `tapes` side
/// effects in one transaction, returning the updated job.
pub async fn patch(
    pool: &SqlitePool,
    id: &str,
    req: &PatchTapeJobRequest,
) -> ApiResult<TapeJobDto> {
    let mut tx = pool.begin().await?;

    let current = sqlx::query_as::<_, TapeJobRow>(&format!("{JOB_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("tape_job", id))?;

    // #132: upload jobs belong to the scheduler-internal ingest task. A worker
    // patching one would corrupt the ingest state machine.
    if current.kind == UPLOAD_KIND {
        return Err(ApiError::conflict(
            "upload_job_not_patchable",
            format!("tape job {id} is an ingest job and cannot be patched via the worker API"),
        ));
    }

    if !transition_allowed(&current.state, &req.state) {
        return Err(ApiError::conflict(
            "invalid_state_transition",
            format!(
                "tape job {id} cannot move from '{}' to '{}'",
                current.state, req.state
            ),
        ));
    }

    if req.state == "done" && current.kind == "probe" && req.duration_ms.is_none() {
        return Err(ApiError::validation(
            "duration_ms",
            "duration_ms is required when a probe job is marked done",
        ));
    }

    let now = now_ts();
    let error = if req.state == "done" {
        None
    } else {
        req.error.clone()
    };

    sqlx::query("UPDATE tape_jobs SET state = ?, error = ?, updated_at = ? WHERE id = ?")
        .bind(&req.state)
        .bind(&error)
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    match (current.kind.as_str(), req.state.as_str()) {
        ("probe", "done") => {
            sqlx::query(
                "UPDATE tapes SET duration_ms = ?, format = COALESCE(?, format), \
                 mime = COALESCE(?, mime), status = 'processing', updated_at = ? WHERE id = ?",
            )
            .bind(req.duration_ms)
            .bind(&req.format)
            .bind(&req.mime)
            .bind(&now)
            .bind(&current.tape_id)
            .execute(&mut *tx)
            .await?;
        }
        ("peaks", "done") => {
            sqlx::query("UPDATE tapes SET status = 'ready', updated_at = ? WHERE id = ?")
                .bind(&now)
                .bind(&current.tape_id)
                .execute(&mut *tx)
                .await?;
        }
        (_, "failed") => {
            sqlx::query("UPDATE tapes SET status = 'failed', updated_at = ? WHERE id = ?")
                .bind(&now)
                .bind(&current.tape_id)
                .execute(&mut *tx)
                .await?;
        }
        // A retry (`pending`) leaves the tape status untouched.
        _ => {}
    }

    tx.commit().await?;

    let row = sqlx::query_as::<_, TapeJobRow>(&format!("{JOB_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(row.into_dto())
}

/// Successful upload metadata written back to `tapes` (#132).
#[derive(Debug, Clone)]
pub struct UploadOutcome {
    /// Content hash of the staging file; doubles as `tapes.music_api_id`.
    pub sha256: String,
    /// File size in bytes.
    pub size_bytes: i64,
    /// `ETag` from the music API, if it returned one.
    pub etag: Option<String>,
}

/// Completes an `upload` job and all of its `tapes` side effects in one
/// transaction (#132):
///
/// - `tapes` → `status='processing'`, `music_api_id`/`storage_key`/`size_bytes`/
///   `etag`/`format='mp3'`/`mime='audio/mpeg'`,
/// - `probe` and `peaks` jobs → `pending` (`ON CONFLICT DO NOTHING`, so a
///   repeated completion cannot duplicate them),
/// - the `upload` job → `done`.
///
/// The caller deletes the staging file only after this returns `Ok`.
pub async fn complete_upload(
    pool: &SqlitePool,
    job_id: &str,
    tape_id: &str,
    outcome: &UploadOutcome,
) -> ApiResult<()> {
    let mut tx = pool.begin().await?;

    let current = sqlx::query_as::<_, TapeJobRow>(&format!("{JOB_SELECT} WHERE id = ?"))
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("tape_job", job_id))?;

    if current.kind != UPLOAD_KIND {
        return Err(ApiError::conflict(
            "not_an_upload_job",
            format!(
                "tape job {job_id} has kind '{}', expected 'upload'",
                current.kind
            ),
        ));
    }
    if !transition_allowed(&current.state, "done") {
        return Err(ApiError::conflict(
            "invalid_state_transition",
            format!(
                "tape job {job_id} cannot move from '{}' to 'done'",
                current.state
            ),
        ));
    }
    if current.tape_id != tape_id {
        return Err(ApiError::conflict(
            "tape_mismatch",
            format!("tape job {job_id} does not belong to tape {tape_id}"),
        ));
    }

    let now = now_ts();
    sqlx::query(
        "UPDATE tapes SET music_api_id = ?, storage_key = ?, size_bytes = ?, etag = ?, \
         format = 'mp3', mime = 'audio/mpeg', status = 'processing', updated_at = ? \
         WHERE id = ?",
    )
    .bind(&outcome.sha256)
    .bind(&outcome.sha256)
    .bind(outcome.size_bytes)
    .bind(&outcome.etag)
    .bind(&now)
    .bind(tape_id)
    .execute(&mut *tx)
    .await?;

    for kind in KINDS {
        sqlx::query(
            "INSERT INTO tape_jobs (id, tape_id, kind, state, attempts, created_at, updated_at) \
             VALUES (?, ?, ?, 'pending', 0, ?, ?) ON CONFLICT (tape_id, kind) DO NOTHING",
        )
        .bind(new_id())
        .bind(tape_id)
        .bind(kind)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("UPDATE tape_jobs SET state = 'done', error = NULL, updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Records an upload failure. A retryable failure moves the job back to
/// `pending` with `error` set (the claim query then applies the usual
/// `attempts` backoff), so the next poll pass retries it automatically. When
/// `terminal` is set (attempt cap reached) the job stays `failed`; the tape
/// row is left untouched either way and the caller keeps the staging file.
/// A `done` job is terminal (`409`).
pub async fn fail_upload(
    pool: &SqlitePool,
    job_id: &str,
    error: &str,
    terminal: bool,
) -> ApiResult<()> {
    let target_state = if terminal { "failed" } else { "pending" };
    let mut tx = pool.begin().await?;

    let current = sqlx::query_as::<_, TapeJobRow>(&format!("{JOB_SELECT} WHERE id = ?"))
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("tape_job", job_id))?;

    if current.kind != UPLOAD_KIND {
        return Err(ApiError::conflict(
            "not_an_upload_job",
            format!(
                "tape job {job_id} has kind '{}', expected 'upload'",
                current.kind
            ),
        ));
    }
    if !transition_allowed(&current.state, target_state) {
        return Err(ApiError::conflict(
            "invalid_state_transition",
            format!(
                "tape job {job_id} cannot move from '{}' to '{target_state}'",
                current.state,
            ),
        ));
    }

    sqlx::query("UPDATE tape_jobs SET state = ?, error = ?, updated_at = ? WHERE id = ?")
        .bind(target_state)
        .bind(error)
        .bind(now_ts())
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Fetches one job or `404` (used by tests and future tooling).
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<TapeJobDto> {
    let sql = format!("{JOB_SELECT} WHERE id = ?");
    sqlx::query_as::<_, TapeJobRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(TapeJobRow::into_dto)
        .ok_or_else(|| ApiError::not_found("tape_job", id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_secs(0), 0);
        assert_eq!(backoff_secs(1), 5);
        assert_eq!(backoff_secs(2), 10);
        assert_eq!(backoff_secs(3), 20);
        assert_eq!(backoff_secs(4), 40);
        assert_eq!(backoff_secs(5), 80);
        assert_eq!(backoff_secs(6), 160);
        assert_eq!(backoff_secs(7), 300);
        assert_eq!(backoff_secs(99), 300);
    }

    #[test]
    fn transitions() {
        assert!(transition_allowed("pending", "running"));
        assert!(transition_allowed("running", "done"));
        assert!(transition_allowed("running", "failed"));
        assert!(transition_allowed("running", "pending"));
        assert!(transition_allowed("failed", "pending"));
        assert!(!transition_allowed("done", "failed"));
        assert!(!transition_allowed("done", "pending"));
        assert!(transition_allowed("running", "running"));
    }
}
