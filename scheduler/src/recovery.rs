//! Startup recovery for stale upload jobs (Issue #18, US-10).
//!
//! A portal upload that dies mid-flight leaves a `running` job behind. On boot
//! the scheduler marks such jobs older than `UPLOAD_RUNNING_TIMEOUT` as either
//! requeued (`pending`, when the job still has retries left) or `failed` with
//! reason `timeout`. There is therefore never a `running` job without a
//! timeout (portal.md §7).

use chrono::{Duration, SecondsFormat, Utc};
use sqlx::SqlitePool;

use crate::dto::PatchUploadJobRequest;
use crate::error::ApiResult;
use crate::store::upload_jobs;

/// Default `UPLOAD_RUNNING_TIMEOUT` in seconds (30 min).
pub const DEFAULT_TIMEOUT_SECS: i64 = 30 * 60;

/// Default maximum attempts before a stale job is failed instead of requeued.
pub const DEFAULT_MAX_ATTEMPTS: i64 = 3;

/// Environment variable overriding [`DEFAULT_TIMEOUT_SECS`] (seconds).
pub const TIMEOUT_ENV: &str = "UPLOAD_RUNNING_TIMEOUT";

/// What a recovery pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Jobs moved back to `pending` (retries remained).
    pub requeued: usize,
    /// Jobs marked `failed` (`timeout`, retries exhausted).
    pub failed: usize,
}

/// Reads `UPLOAD_RUNNING_TIMEOUT` (seconds); falls back to the default.
pub fn timeout_secs_from_env() -> i64 {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

/// RFC3339 UTC ms cutoff: `now - timeout_secs` (§3.1).
fn cutoff_ts(timeout_secs: i64) -> String {
    (Utc::now() - Duration::seconds(timeout_secs)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Recovers stale `running` upload jobs. Returns what was changed.
pub async fn recover_stale_uploads(
    pool: &SqlitePool,
    timeout_secs: i64,
    max_attempts: i64,
) -> ApiResult<RecoveryReport> {
    let cutoff = cutoff_ts(timeout_secs);
    let stale = upload_jobs::list_stale_running(pool, &cutoff).await?;
    let mut report = RecoveryReport::default();

    for job in stale {
        if job.attempts < max_attempts {
            upload_jobs::patch_state(
                pool,
                &job.id,
                &PatchUploadJobRequest {
                    state: "pending".into(),
                    sha256: None,
                    track_id: None,
                    error: Some("requeued after timeout".into()),
                },
            )
            .await?;
            report.requeued += 1;
        } else {
            upload_jobs::patch_state(
                pool,
                &job.id,
                &PatchUploadJobRequest {
                    state: "failed".into(),
                    sha256: None,
                    track_id: None,
                    error: Some("timeout".into()),
                },
            )
            .await?;
            report.failed += 1;
        }
    }

    Ok(report)
}
