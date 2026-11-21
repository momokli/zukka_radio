//! Tape-recording ingest (#132): staged MP3 → music API → `tapes` → `tape_jobs`.
//!
//! Liquidsoap writes `output.file` to `/recordings/<tape_id>.mp3` and the
//! scheduler owns everything after the recording stops (§3.6). This module
//! drives the `upload` job:
//!
//! 1. hash the staging file streaming (never buffered into RAM),
//! 2. `HEAD /objects/{sha256}` — an existing object is reused (dedupe),
//! 3. otherwise `PUT /objects/{sha256}` with the file as a request stream,
//! 4. [`store::tape_jobs::complete_upload`] fills `tapes` and enqueues
//!    `probe`/`peaks` in one transaction,
//! 5. only then delete the staging file.
//!
//! Failures go through [`store::tape_jobs::fail_upload`]: the job stays
//! retryable with `error` set, the tape stays `pending` and the file is kept.
//! After [`MAX_UPLOAD_ATTEMPTS`] the tape is marked `failed`; the file stays for
//! manual recovery.
//!
//! The loop is started from `main.rs` next to the announcement dispatcher and
//! ends with the process. Without `MUSIC_API_BASE_URL`/`MUSIC_API_TOKEN` it is a
//! logged no-op.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;

use crate::dto::TapeJobClaimDto;
use crate::error::ApiResult;
use crate::store::now_ts;
use crate::store::tape_jobs::{self, MAX_UPLOAD_ATTEMPTS, UPLOAD_KIND, UploadOutcome};

/// Env var for the music API base URL (no `/api/v1` prefix).
pub const BASE_URL_ENV: &str = "MUSIC_API_BASE_URL";
/// Env var for the bearer token used against the music API.
pub const TOKEN_ENV: &str = "MUSIC_API_TOKEN";
/// Env var for the staging directory; defaults to the Liquidsoap mount.
pub const STAGING_ROOT_ENV: &str = "TAPES_STAGING_DIR";
/// Default staging directory (`/recordings` inside the container).
pub const DEFAULT_STAGING_ROOT: &str = "/recordings";
/// Poll interval override, in seconds.
pub const POLL_SECS_ENV: &str = "TAPE_INGEST_POLL_SECS";

const DEFAULT_POLL_SECS: u64 = 10;
const CLAIM_LIMIT: i64 = 4;
const CLAIM_LEASE_SECS: i64 = 600;
const READ_BUFFER: usize = 64 * 1024;

/// Runtime configuration, injectable so tests need no process environment.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub music_api_base: String,
    pub token: String,
    pub staging_root: PathBuf,
}

impl IngestConfig {
    /// Reads the config from the process environment. `None` when either the
    /// base URL or the token is missing/empty (ingest disabled).
    pub fn from_env() -> Option<Self> {
        let base = std::env::var(BASE_URL_ENV).ok()?.trim().to_string();
        let token = std::env::var(TOKEN_ENV).ok()?.trim().to_string();
        if base.is_empty() || token.is_empty() {
            return None;
        }
        let root = std::env::var(STAGING_ROOT_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_STAGING_ROOT.to_string());
        Some(Self {
            music_api_base: base,
            token,
            staging_root: PathBuf::from(root),
        })
    }
}

/// Outcome of one poll pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Report {
    pub uploaded: usize,
    pub deduped: usize,
    pub failed: usize,
}

impl Report {
    fn count(&self) -> usize {
        self.uploaded + self.deduped + self.failed
    }
}

/// Staging file for a tape: `{root}/{tape_id}.mp3`.
pub fn staging_path(root: &Path, tape_id: &str) -> PathBuf {
    root.join(format!("{tape_id}.mp3"))
}

/// `X-Original-Path` header value; the canonical location inside the container.
pub fn original_path(tape_id: &str) -> String {
    format!("/recordings/{tape_id}.mp3")
}

enum JobOutcome {
    Uploaded,
    Deduped,
    Failed,
}

/// Claims every due `upload` job once and processes it.
pub async fn run_once(pool: &SqlitePool, cfg: &IngestConfig) -> ApiResult<Report> {
    let kinds = [UPLOAD_KIND.to_string()];
    let jobs = tape_jobs::claim(pool, &kinds, CLAIM_LIMIT, CLAIM_LEASE_SECS).await?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| crate::error::ApiError::Internal(anyhow::anyhow!("http client: {err}")))?;

    let mut report = Report::default();
    for job in jobs {
        match handle_job(pool, cfg, &client, &job).await {
            JobOutcome::Uploaded => report.uploaded += 1,
            JobOutcome::Deduped => report.deduped += 1,
            JobOutcome::Failed => report.failed += 1,
        }
    }
    Ok(report)
}

/// Background poll loop; the task ends with the process.
pub fn spawn(pool: SqlitePool, cfg: IngestConfig) -> JoinHandle<()> {
    let secs = std::env::var(POLL_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_POLL_SECS);
    tracing::info!(
        interval_secs = secs,
        staging_root = %cfg.staging_root.display(),
        "tape ingest enabled"
    );

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match run_once(&pool, &cfg).await {
                Ok(report) if report.count() > 0 => tracing::info!(
                    uploaded = report.uploaded,
                    deduped = report.deduped,
                    failed = report.failed,
                    "tape ingest pass"
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!("tape ingest pass failed: {err}"),
            }
        }
    })
}

async fn handle_job(
    pool: &SqlitePool,
    cfg: &IngestConfig,
    client: &reqwest::Client,
    job: &TapeJobClaimDto,
) -> JobOutcome {
    let path = staging_path(&cfg.staging_root, &job.tape_id);
    if !path.is_file() {
        return fail(
            pool,
            job,
            format!("staging file missing: {}", path.display()),
        )
        .await;
    }

    let sha256 = match sha256_file(&path).await {
        Ok(value) => value,
        Err(err) => {
            return fail(pool, job, format!("sha256 failed: {err}")).await;
        }
    };
    let size_bytes = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len() as i64,
        Err(err) => {
            return fail(pool, job, format!("stat failed: {err}")).await;
        }
    };

    let url = object_url(&cfg.music_api_base, &sha256);

    // Dedupe: an already uploaded object needs no second PUT.
    let exists = match client.head(&url).bearer_auth(&cfg.token).send().await {
        Ok(response) => match response.status() {
            StatusCode::OK => true,
            StatusCode::NOT_FOUND => false,
            status => {
                return fail(pool, job, format!("music api HEAD returned {status}")).await;
            }
        },
        Err(err) => return fail(pool, job, format!("music api HEAD failed: {err}")).await,
    };

    let mut etag: Option<String> = None;
    if !exists {
        let file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(err) => {
                return fail(pool, job, format!("open staging file failed: {err}")).await;
            }
        };
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        match client
            .put(&url)
            .bearer_auth(&cfg.token)
            .header("X-Name", &job.slug)
            .header("X-Original-Path", original_path(&job.tape_id))
            .header("Content-Type", "audio/mpeg")
            .header(reqwest::header::CONTENT_LENGTH, size_bytes)
            .body(body)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    return fail(pool, job, format!("music api PUT returned {status}")).await;
                }
                etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
            }
            Err(err) => return fail(pool, job, format!("music api PUT failed: {err}")).await,
        }
    }

    // Fallback (R-C): the hash is a stable object identity when the API sends
    // no ETag.
    let outcome = UploadOutcome {
        etag: etag.or_else(|| Some(sha256.clone())),
        sha256,
        size_bytes,
    };

    if let Err(err) = tape_jobs::complete_upload(pool, &job.id, &job.tape_id, &outcome).await {
        return fail(pool, job, format!("complete_upload failed: {err}")).await;
    }

    // Only after the DB commit: the file is the recovery source on failure.
    if let Err(err) = tokio::fs::remove_file(&path).await {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(tape_id = %job.tape_id, "failed to delete staging file: {err}");
        }
    }

    if exists {
        JobOutcome::Deduped
    } else {
        JobOutcome::Uploaded
    }
}

fn object_url(base: &str, sha256: &str) -> String {
    format!("{}/objects/{}", base.trim_end_matches('/'), sha256)
}

async fn fail(pool: &SqlitePool, job: &TapeJobClaimDto, message: String) -> JobOutcome {
    tracing::warn!(tape_id = %job.tape_id, job_id = %job.id, "tape ingest failed: {message}");
    let terminal = job.attempts >= MAX_UPLOAD_ATTEMPTS;
    if let Err(err) = tape_jobs::fail_upload(pool, &job.id, &message, terminal).await {
        tracing::warn!(job_id = %job.id, "failed to record upload failure: {err}");
    }
    if terminal {
        let result = sqlx::query(
            "UPDATE tapes SET status = 'failed', updated_at = ? WHERE id = ? AND status = 'pending'",
        )
        .bind(now_ts())
        .bind(&job.tape_id)
        .execute(pool)
        .await;
        if let Err(err) = result {
            tracing::warn!(tape_id = %job.tape_id, "failed to mark tape failed: {err}");
        }
    }
    JobOutcome::Failed
}

/// Streaming SHA-256 of a file; hex-encoded, 64 KiB buffer, never in RAM.
pub async fn sha256_file(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streaming_sha256_matches_known_vector() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tape.mp3");
        tokio::fs::write(&path, b"hello zukka")
            .await
            .expect("write");
        assert_eq!(
            sha256_file(&path).await.expect("hash"),
            "3906f486a8fdf4ebebdece160621598c37057af3f5baad389dd00053f64be8b9"
        );
    }

    #[tokio::test]
    async fn sha256_streams_large_files_in_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.mp3");
        let data = vec![0xABu8; READ_BUFFER * 3 + 7];
        tokio::fs::write(&path, &data).await.expect("write");
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(&data);
            hex(&hasher.finalize())
        };
        assert_eq!(sha256_file(&path).await.expect("hash"), expected);
    }

    #[test]
    fn staging_path_uses_mp3_suffix() {
        let root = Path::new("/recordings");
        assert_eq!(
            staging_path(root, "0192aaaa-0001-7000-8000-00000000000a").to_string_lossy(),
            "/recordings/0192aaaa-0001-7000-8000-00000000000a.mp3"
        );
        assert_eq!(
            original_path("0192aaaa-0001-7000-8000-00000000000a"),
            "/recordings/0192aaaa-0001-7000-8000-00000000000a.mp3"
        );
    }

    #[test]
    fn object_url_strips_trailing_slash() {
        assert_eq!(
            object_url("http://api:8080/", "abc"),
            "http://api:8080/objects/abc"
        );
        assert_eq!(
            object_url("http://api:8080", "abc"),
            "http://api:8080/objects/abc"
        );
    }
}
