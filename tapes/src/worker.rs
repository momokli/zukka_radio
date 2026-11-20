//! Waveform worker: claims `tape_jobs`, runs ffprobe/ffmpeg, writes results
//! back through the scheduler (Issue #25, M3, P2/§2.2/§4.2).
//!
//! The worker never touches SQLite. Every write goes through
//! `POST /internal/tape-jobs/claim`, `PATCH /internal/tape-jobs/{id}` and
//! `POST /internal/waveforms`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::waveform::{
    self, ALGORITHM, CHUNK_SIZE, MAX_BARS, MS_PER_BAR, ProbeResult, RmsBarEncoder, SCHEME,
    WaveformError,
};

/// Error type for the worker loop / backend.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("scheduler: {0}")]
    Scheduler(String),
    #[error("config: {0}")]
    Config(String),
    #[error(transparent)]
    Waveform(#[from] WaveformError),
}

/// Backend failure (ffmpeg/ffprobe/music object).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

/// One claimed job as returned by the scheduler.
#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    pub id: String,
    pub tape_id: String,
    pub kind: String,
    pub attempts: i64,
    #[allow(dead_code)]
    pub slug: String,
    pub storage_key: Option<String>,
    #[allow(dead_code)]
    pub source: String,
}

/// Worker → scheduler patch body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatchJob {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PatchJob {
    fn done_probe(result: &ProbeResult) -> Self {
        Self {
            state: "done".into(),
            duration_ms: Some(result.duration_ms),
            format: Some(result.format.clone()),
            mime: Some(result.mime.clone()),
            error: None,
        }
    }

    fn done() -> Self {
        Self {
            state: "done".into(),
            duration_ms: None,
            format: None,
            mime: None,
            error: None,
        }
    }

    fn retry(message: &str) -> Self {
        Self {
            state: "pending".into(),
            duration_ms: None,
            format: None,
            mime: None,
            error: Some(message.into()),
        }
    }

    fn failed(message: &str) -> Self {
        Self {
            state: "failed".into(),
            duration_ms: None,
            format: None,
            mime: None,
            error: Some(message.into()),
        }
    }
}

/// Waveform upsert body for `POST /internal/waveforms`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WaveformUpload {
    pub subject_kind: String,
    pub subject_id: String,
    pub scheme: i64,
    pub algorithm: String,
    pub ms_per_bar: i64,
    pub bars: i64,
    pub data: Vec<u8>,
}

/// Scheduler write-through client.
pub struct SchedulerClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl SchedulerClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/internal{path}", self.base_url)
    }

    async fn check(response: reqwest::Response) -> Result<reqwest::Response, WorkerError> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(WorkerError::Scheduler(format!(
            "{} {}",
            status,
            body.trim()
        )))
    }
}

#[derive(Debug, Serialize)]
struct ClaimRequest {
    kinds: Vec<String>,
    limit: i64,
    lease_secs: i64,
}

#[derive(Debug, Deserialize)]
struct ClaimResponse {
    jobs: Vec<Claim>,
}

#[async_trait]
impl JobSource for SchedulerClient {
    async fn claim(
        &self,
        kinds: &[&str],
        limit: i64,
        lease_secs: i64,
    ) -> Result<Vec<Claim>, WorkerError> {
        let body = ClaimRequest {
            kinds: kinds.iter().map(|kind| (*kind).to_string()).collect(),
            limit,
            lease_secs,
        };
        let response = self
            .client
            .post(self.url("/tape-jobs/claim"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        let response = Self::check(response).await?;
        let parsed: ClaimResponse = response.json().await?;
        Ok(parsed.jobs)
    }

    async fn patch(&self, id: &str, patch: &PatchJob) -> Result<(), WorkerError> {
        let response = self
            .client
            .patch(self.url(&format!("/tape-jobs/{id}")))
            .bearer_auth(&self.token)
            .json(patch)
            .send()
            .await?;
        Self::check(response).await?;
        Ok(())
    }

    async fn upload_waveform(&self, upload: &WaveformUpload) -> Result<(), WorkerError> {
        let response = self
            .client
            .post(self.url("/waveforms"))
            .bearer_auth(&self.token)
            .json(upload)
            .send()
            .await?;
        Self::check(response).await?;
        Ok(())
    }
}

/// Write path used by [`Worker`] (mockable in tests).
#[async_trait]
pub trait JobSource: Send + Sync {
    async fn claim(
        &self,
        kinds: &[&str],
        limit: i64,
        lease_secs: i64,
    ) -> Result<Vec<Claim>, WorkerError>;
    async fn patch(&self, id: &str, patch: &PatchJob) -> Result<(), WorkerError>;
    async fn upload_waveform(&self, upload: &WaveformUpload) -> Result<(), WorkerError>;
}

/// Decoding backend (mockable in tests; ffmpeg in production).
#[async_trait]
pub trait WaveformBackend: Send + Sync {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, BackendError>;
    async fn peaks(&self, storage_key: &str) -> Result<Vec<u8>, BackendError>;
}

/// ffmpeg/ffprobe backend reading the Music API object directly.
pub struct FfmpegBackend {
    music_api_url: String,
    music_api_token: String,
    ffmpeg_bin: String,
    ffprobe_bin: String,
    timeout: Duration,
}

impl FfmpegBackend {
    pub fn from_config(config: &Config) -> Self {
        Self {
            music_api_url: config.music_api_url.clone(),
            music_api_token: config.music_api_token.clone(),
            ffmpeg_bin: config.ffmpeg_bin.clone(),
            ffprobe_bin: config.ffprobe_bin.clone(),
            timeout: Duration::from_secs(config.ffmpeg_timeout_secs.max(1)),
        }
    }

    fn object_url(&self, storage_key: &str) -> String {
        format!(
            "{}/objects/{}",
            self.music_api_url.trim_end_matches('/'),
            storage_key.trim_start_matches('/')
        )
    }

    fn auth(&self) -> Option<String> {
        waveform::auth_header(&self.music_api_token)
    }
}

#[async_trait]
impl WaveformBackend for FfmpegBackend {
    async fn probe(&self, storage_key: &str) -> Result<ProbeResult, BackendError> {
        let url = self.object_url(storage_key);
        let auth = self.auth();
        waveform::probe_media(&self.ffprobe_bin, &url, auth.as_deref(), self.timeout)
            .await
            .map_err(|err| BackendError(err.to_string()))
    }

    async fn peaks(&self, storage_key: &str) -> Result<Vec<u8>, BackendError> {
        use tokio::io::AsyncReadExt;

        let url = self.object_url(storage_key);
        let auth = self.auth();
        let mut command = waveform::decode_command(&self.ffmpeg_bin, &url, auth.as_deref());
        let mut child = command
            .spawn()
            .map_err(|err| BackendError(format!("failed to spawn ffmpeg: {err}")))?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| BackendError("ffmpeg stdout unavailable".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| BackendError("ffmpeg stderr unavailable".into()))?;

        let stderr_task = tokio::spawn(async move {
            let mut buffer = String::new();
            let _ = stderr.read_to_string(&mut buffer).await;
            buffer
        });

        let streamed = tokio::time::timeout(self.timeout, async {
            let mut encoder = RmsBarEncoder::new();
            let mut buffer = vec![0u8; CHUNK_SIZE];
            let mut produced: i64 = 0;
            loop {
                let read = stdout.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                let mut bars = encoder.push_chunk(&buffer[..read]);
                produced += bars.len() as i64;
                if produced > MAX_BARS {
                    return Err(WaveformError::TooLong(produced));
                }
                bars.clear();
            }
            let mut out = encoder.finish();
            if out.len() as i64 > MAX_BARS {
                return Err(WaveformError::TooLong(out.len() as i64));
            }
            Ok::<Vec<u8>, WaveformError>(std::mem::take(&mut out))
        })
        .await;

        let timed_out = streamed.is_err();
        if timed_out {
            let _ = child.kill().await;
        }
        let status = child
            .wait()
            .await
            .map_err(|err| BackendError(format!("ffmpeg wait failed: {err}")))?;
        let stderr = stderr_task.await.unwrap_or_default();

        match streamed {
            Ok(Ok(bytes)) => {
                if status.success() {
                    Ok(bytes)
                } else {
                    Err(BackendError(format!(
                        "ffmpeg exit {}: {}",
                        status.code().unwrap_or(-1),
                        stderr.trim()
                    )))
                }
            }
            Ok(Err(err)) => Err(BackendError(err.to_string())),
            Err(_) => Err(BackendError("ffmpeg timed out".into())),
        }
    }
}

/// The job loop.
pub struct Worker {
    source: Arc<dyn JobSource>,
    backend: Arc<dyn WaveformBackend>,
    max_attempts: i64,
    lease_secs: i64,
    poll_interval: Duration,
}

impl Worker {
    pub fn new(
        source: Arc<dyn JobSource>,
        backend: Arc<dyn WaveformBackend>,
        max_attempts: i64,
        lease_secs: i64,
        poll_interval: Duration,
    ) -> Self {
        Self {
            source,
            backend,
            max_attempts,
            lease_secs,
            poll_interval,
        }
    }

    /// Claims a batch and processes each job sequentially; returns the number
    /// of claimed jobs. Job failures are logged and reflected via patching.
    pub async fn run_once(&self) -> Result<usize, WorkerError> {
        let jobs = self
            .source
            .claim(&["probe", "peaks"], 4, self.lease_secs)
            .await?;
        let count = jobs.len();
        for job in jobs {
            if let Err(err) = self.handle(&job).await {
                tracing::warn!(
                    tape_id = %job.tape_id,
                    kind = %job.kind,
                    attempt = job.attempts,
                    "tape job handling failed: {err:#}"
                );
            }
        }
        Ok(count)
    }

    /// Runs until the process is stopped.
    pub async fn run(self) -> Result<(), WorkerError> {
        tracing::info!(
            "tapes worker started (max_attempts {}, lease {}s)",
            self.max_attempts,
            self.lease_secs
        );
        loop {
            match self.run_once().await {
                Ok(0) => tokio::time::sleep(self.poll_interval).await,
                Ok(count) => tracing::debug!("processed {count} tape job(s)"),
                Err(err) => {
                    tracing::warn!("claim failed: {err:#}");
                    tokio::time::sleep(self.poll_interval).await;
                }
            }
        }
    }

    async fn handle(&self, job: &Claim) -> Result<(), WorkerError> {
        let storage_key = job.storage_key.as_deref().ok_or_else(|| {
            WorkerError::Config(format!("tape {} has no storage_key", job.tape_id))
        })?;

        match job.kind.as_str() {
            "probe" => match self.backend.probe(storage_key).await {
                Ok(result) => {
                    tracing::info!(
                        tape_id = %job.tape_id,
                        attempt = job.attempts,
                        duration_ms = result.duration_ms,
                        format = %result.format,
                        "probe done"
                    );
                    self.source
                        .patch(&job.id, &PatchJob::done_probe(&result))
                        .await
                }
                Err(err) => self.retry(job, &err.to_string()).await,
            },
            "peaks" => match self.backend.peaks(storage_key).await {
                Ok(data) => {
                    let bars = data.len() as i64;
                    if bars > MAX_BARS {
                        return self.retry(job, &format!("too many bars: {bars}")).await;
                    }
                    let upload = WaveformUpload {
                        subject_kind: "tape".into(),
                        subject_id: job.tape_id.clone(),
                        scheme: SCHEME,
                        algorithm: ALGORITHM.into(),
                        ms_per_bar: MS_PER_BAR,
                        bars,
                        data,
                    };
                    match self.source.upload_waveform(&upload).await {
                        Ok(()) => {
                            tracing::info!(
                                tape_id = %job.tape_id,
                                attempt = job.attempts,
                                bars,
                                "peaks done"
                            );
                            self.source.patch(&job.id, &PatchJob::done()).await
                        }
                        Err(err) => {
                            self.retry(job, &format!("waveform upload failed: {err}"))
                                .await
                        }
                    }
                }
                Err(err) => self.retry(job, &err.to_string()).await,
            },
            other => Err(WorkerError::Config(format!("unknown job kind '{other}'"))),
        }
    }

    async fn retry(&self, job: &Claim, message: &str) -> Result<(), WorkerError> {
        let message = truncate_message(message);
        if job.attempts < self.max_attempts {
            tracing::warn!(
                tape_id = %job.tape_id,
                kind = %job.kind,
                attempt = job.attempts,
                "retrying tape job: {message}"
            );
            self.source.patch(&job.id, &PatchJob::retry(&message)).await
        } else {
            tracing::error!(
                tape_id = %job.tape_id,
                kind = %job.kind,
                attempt = job.attempts,
                "tape job failed after max attempts: {message}"
            );
            self.source
                .patch(&job.id, &PatchJob::failed(&message))
                .await
        }
    }
}

fn truncate_message(message: &str) -> String {
    const MAX: usize = 2_000;
    if message.len() <= MAX {
        message.to_string()
    } else {
        let mut end = MAX;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &message[..end])
    }
}

/// Fail-fast prerequisites for `zukka-tapes work`.
pub fn check_prerequisites(config: &Config) -> Result<(), WorkerError> {
    if config.music_api_token.trim().is_empty() {
        return Err(WorkerError::Config(
            "MUSIC_API_TOKEN must be set to run the worker".into(),
        ));
    }
    if config.scheduler_token.trim().is_empty() {
        return Err(WorkerError::Config(
            "SCHEDULER_SERVICE_TOKEN (or ZUKKA_SCHEDULER_SERVICE_TOKEN) must be set".into(),
        ));
    }
    for (name, bin) in [
        ("FFMPEG_BIN", &config.ffmpeg_bin),
        ("FFPROBE_BIN", &config.ffprobe_bin),
    ] {
        let status = std::process::Command::new(bin)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !status.map(|value| value.success()).unwrap_or(false) {
            return Err(WorkerError::Config(format!(
                "{name} '{bin}' is not executable"
            )));
        }
    }
    Ok(())
}

/// Builds the production worker from the environment config.
pub fn build(config: &Config) -> Worker {
    Worker::new(
        Arc::new(SchedulerClient::new(
            config.scheduler_url.clone(),
            config.scheduler_token.clone(),
        )),
        Arc::new(FfmpegBackend::from_config(config)),
        config.worker_max_attempts.max(1),
        config.worker_lease_secs.max(1),
        Duration::from_secs(config.worker_poll_secs.max(1)),
    )
}

/// Entry point used by `zukka-tapes work` (prerequisites are checked first).
pub async fn run(config: Config) -> Result<(), WorkerError> {
    check_prerequisites(&config)?;
    build(&config).run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct FakeSource {
        jobs: Mutex<VecDeque<Claim>>,
        patches: Mutex<Vec<(String, PatchJob)>>,
        uploads: Mutex<Vec<WaveformUpload>>,
        fail_upload: Mutex<bool>,
    }

    #[async_trait]
    impl JobSource for FakeSource {
        async fn claim(
            &self,
            _kinds: &[&str],
            _limit: i64,
            _lease_secs: i64,
        ) -> Result<Vec<Claim>, WorkerError> {
            Ok(self.jobs.lock().await.drain(..).collect())
        }

        async fn patch(&self, id: &str, patch: &PatchJob) -> Result<(), WorkerError> {
            self.patches
                .lock()
                .await
                .push((id.to_string(), patch.clone()));
            Ok(())
        }

        async fn upload_waveform(&self, upload: &WaveformUpload) -> Result<(), WorkerError> {
            if *self.fail_upload.lock().await {
                return Err(WorkerError::Scheduler("upload rejected".into()));
            }
            self.uploads.lock().await.push(upload.clone());
            Ok(())
        }
    }

    struct FakeBackend {
        probe: Result<ProbeResult, String>,
        peaks: Result<Vec<u8>, String>,
    }

    #[async_trait]
    impl WaveformBackend for FakeBackend {
        async fn probe(&self, _storage_key: &str) -> Result<ProbeResult, BackendError> {
            self.probe.clone().map_err(BackendError)
        }

        async fn peaks(&self, _storage_key: &str) -> Result<Vec<u8>, BackendError> {
            self.peaks.clone().map_err(BackendError)
        }
    }

    fn claim(id: &str, kind: &str, attempts: i64) -> Claim {
        Claim {
            id: id.into(),
            tape_id: "tape-1".into(),
            kind: kind.into(),
            attempts,
            slug: "slug1".into(),
            storage_key: Some("recordings/a.mp3".into()),
            source: "live".into(),
        }
    }

    fn worker(source: Arc<FakeSource>, backend: FakeBackend) -> Worker {
        Worker::new(source, Arc::new(backend), 5, 600, Duration::from_millis(1))
    }

    fn ok_backend() -> FakeBackend {
        FakeBackend {
            probe: Ok(ProbeResult {
                duration_ms: 7_194_000,
                format: "mp3".into(),
                mime: "audio/mpeg".into(),
            }),
            peaks: Ok(vec![0, 128, 255]),
        }
    }

    #[tokio::test]
    async fn happy_path_probe_then_peaks() {
        let source = Arc::new(FakeSource::default());
        source.jobs.lock().await.push_back(claim("j1", "probe", 1));
        source.jobs.lock().await.push_back(claim("j2", "peaks", 1));
        let worker = worker(source.clone(), ok_backend());

        assert_eq!(worker.run_once().await.unwrap(), 2);

        let patches = source.patches.lock().await;
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].1.state, "done");
        assert_eq!(patches[0].1.duration_ms, Some(7_194_000));
        assert_eq!(patches[0].1.format.as_deref(), Some("mp3"));
        assert_eq!(patches[1].1.state, "done");

        let uploads = source.uploads.lock().await;
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].subject_kind, "tape");
        assert_eq!(uploads[0].subject_id, "tape-1");
        assert_eq!(uploads[0].bars, 3);
        assert_eq!(uploads[0].ms_per_bar, 500);
        assert_eq!(uploads[0].scheme, 1);
        assert_eq!(uploads[0].algorithm, ALGORITHM);
    }

    #[tokio::test]
    async fn probe_error_retries_while_attempts_remain() {
        let source = Arc::new(FakeSource::default());
        source.jobs.lock().await.push_back(claim("j1", "probe", 4));
        let mut backend = ok_backend();
        backend.probe = Err("corrupt media".into());
        let worker = worker(source.clone(), backend);

        worker.run_once().await.unwrap();

        let patches = source.patches.lock().await;
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].1.state, "pending");
        assert_eq!(patches[0].1.error.as_deref(), Some("corrupt media"));
        assert!(source.uploads.lock().await.is_empty());
    }

    #[tokio::test]
    async fn probe_error_fails_at_attempt_cap() {
        let source = Arc::new(FakeSource::default());
        source.jobs.lock().await.push_back(claim("j1", "probe", 5));
        let mut backend = ok_backend();
        backend.probe = Err("corrupt media".into());
        let worker = worker(source.clone(), backend);

        worker.run_once().await.unwrap();

        let patches = source.patches.lock().await;
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].1.state, "failed");
        assert_eq!(patches[0].1.error.as_deref(), Some("corrupt media"));
    }

    #[tokio::test]
    async fn upload_failure_retries() {
        let source = Arc::new(FakeSource::default());
        *source.fail_upload.lock().await = true;
        source.jobs.lock().await.push_back(claim("j2", "peaks", 2));
        let worker = worker(source.clone(), ok_backend());

        worker.run_once().await.unwrap();

        let patches = source.patches.lock().await;
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].1.state, "pending");
        assert!(patches[0].1.error.as_deref().unwrap().contains("upload"));
    }

    #[tokio::test]
    async fn backend_error_retries_peaks() {
        let source = Arc::new(FakeSource::default());
        source.jobs.lock().await.push_back(claim("j2", "peaks", 5));
        let mut backend = ok_backend();
        backend.peaks = Err("ffmpeg exit 1".into());
        let worker = worker(source.clone(), backend);

        worker.run_once().await.unwrap();

        let patches = source.patches.lock().await;
        assert_eq!(patches[0].1.state, "failed");
    }

    #[tokio::test]
    async fn empty_claim_returns_zero() {
        let source = Arc::new(FakeSource::default());
        let worker = worker(source, ok_backend());
        assert_eq!(worker.run_once().await.unwrap(), 0);
    }

    #[test]
    fn messages_are_bounded() {
        let long = "x".repeat(4_000);
        assert!(truncate_message(&long).len() <= 2_003);
    }
}
