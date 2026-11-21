//! Integration tests for the upload pipeline (Issue #18: US-5, US-8, US-9).
//!
//! The Music API and the scheduler job API are replaced by in-memory fakes;
//! `ffprobe`/`ffmpeg` are real (tests skip when the binaries are missing). The
//! multipart staging is exercised through a tiny axum router.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use askama::Template;
use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Multipart, State};
use axum::http::{Method, Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use zukka_portal::api_client::{
    FakeTrackApi, JobError, NewUploadJob, NewWaveform, UploadJob, UploadJobApi, UploadJobPatch,
    Waveform, WaveformApi, WaveformApiError,
};
use zukka_portal::error::PortalError;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::templates::{LibraryPage, UploadJobFragment};
use zukka_portal::upload::{StagedUpload, stage_multipart};
use zukka_portal::upload_pipeline::{PipelineDeps, PipelineError, run};

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeJobsInner {
    jobs: Vec<UploadJob>,
    counter: u64,
}

/// In-memory [`UploadJobApi`].
#[derive(Default)]
struct FakeJobs {
    inner: Mutex<FakeJobsInner>,
}

impl FakeJobs {
    fn new() -> Self {
        Self::default()
    }

    fn state_of(&self, id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.state.clone())
    }

    fn job_of(&self, id: &str) -> Option<UploadJob> {
        self.inner
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }
}

#[async_trait]
impl UploadJobApi for FakeJobs {
    async fn create_job(&self, job: &NewUploadJob) -> Result<String, JobError> {
        let mut inner = self.inner.lock().unwrap();
        inner.counter += 1;
        let id = format!("job-{}", inner.counter);
        inner.jobs.push(UploadJob {
            id: id.clone(),
            caster_id: Some(job.caster_id.clone()),
            filename: job.filename.clone(),
            size_bytes: job.size_bytes,
            sha256: job.sha256.clone(),
            state: "pending".into(),
            track_id: None,
            error: None,
            attempts: 0,
            created_at: "2026-11-18T12:00:00.000Z".into(),
            updated_at: "2026-11-18T12:00:00.000Z".into(),
        });
        Ok(id)
    }

    async fn patch_job(&self, id: &str, patch: &UploadJobPatch) -> Result<UploadJob, JobError> {
        let mut inner = self.inner.lock().unwrap();
        let job = inner
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(JobError::not_found)?;
        job.state = patch.state.clone();
        if let Some(sha) = &patch.sha256 {
            job.sha256 = Some(sha.clone());
        }
        if let Some(track) = &patch.track_id {
            job.track_id = Some(track.clone());
        }
        job.error = patch.error.clone();
        if patch.state == "running" {
            job.attempts += 1;
        }
        Ok(job.clone())
    }

    async fn get_job(&self, id: &str) -> Result<UploadJob, JobError> {
        self.job_of(id).ok_or_else(JobError::not_found)
    }

    async fn list_jobs(&self, _limit: i64) -> Result<Vec<UploadJob>, JobError> {
        Ok(self.inner.lock().unwrap().jobs.clone())
    }
}

// ---------------------------------------------------------------------------
// Fake waveforms
// ---------------------------------------------------------------------------

/// In-memory [`WaveformApi`] recording every upsert.
#[derive(Default)]
struct FakeWaveforms {
    written: Mutex<Vec<NewWaveform>>,
}

impl FakeWaveforms {
    fn new() -> Self {
        Self::default()
    }

    fn count(&self) -> usize {
        self.written.lock().unwrap().len()
    }

    fn last(&self) -> Option<NewWaveform> {
        self.written.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl WaveformApi for FakeWaveforms {
    async fn put_waveform(&self, waveform: &NewWaveform) -> Result<Waveform, WaveformApiError> {
        self.written.lock().unwrap().push(waveform.clone());
        Ok(Waveform {
            subject_kind: waveform.subject_kind.clone(),
            subject_id: waveform.subject_id.clone(),
            scheme: waveform.scheme,
            algorithm: waveform.algorithm.clone(),
            ms_per_bar: waveform.ms_per_bar,
            bars: waveform.bars,
            data: waveform.data.clone(),
            created_at: "2026-11-18T12:00:00.000Z".into(),
        })
    }

    async fn get_waveform(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError> {
        Ok(self
            .written
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.subject_kind == subject_kind && w.subject_id == subject_id)
            .map(|w| Waveform {
                subject_kind: w.subject_kind.clone(),
                subject_id: w.subject_id.clone(),
                scheme: w.scheme,
                algorithm: w.algorithm.clone(),
                ms_per_bar: w.ms_per_bar,
                bars: w.bars,
                data: w.data.clone(),
                created_at: "2026-11-18T12:00:00.000Z".into(),
            }))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_audio(dir: &Path, name: &str, extra: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.5",
        ])
        .args(extra)
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg generate {name}");
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Stages raw `bytes` as a [`StagedUpload`] (mimics the streaming stage step).
fn stage_bytes(dir: &Path, filename: &str, content_type: &str, bytes: &[u8]) -> StagedUpload {
    let named = tempfile::Builder::new()
        .prefix("stage-")
        .tempfile_in(dir)
        .unwrap();
    let path = named.into_temp_path();
    std::fs::write(&path, bytes).unwrap();
    StagedUpload {
        field_name: "file".into(),
        filename: filename.into(),
        content_type: content_type.into(),
        path,
        size_bytes: bytes.len() as i64,
        sha256: sha256_hex(bytes),
        isrc: None,
    }
}

fn stage_file(dir: &Path, filename: &str, content_type: &str, path: &Path) -> StagedUpload {
    let bytes = std::fs::read(path).unwrap();
    stage_bytes(dir, filename, content_type, &bytes)
}

// ---------------------------------------------------------------------------
// US-5 — streaming multipart staging
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct StageState {
    dir: PathBuf,
    max: i64,
}

async fn stage_handler(
    State(state): State<StageState>,
    mut multipart: Multipart,
) -> Result<Json<Value>, PortalError> {
    let staged = stage_multipart(&mut multipart, &state.dir, state.max).await?;
    let out: Vec<Value> = staged
        .iter()
        .map(|s| json!({"filename": s.filename, "size": s.size_bytes, "sha256": s.sha256}))
        .collect();
    Ok(Json(json!(out)))
}

fn multipart_body(boundary: &str, parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, data) in parts {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; \
                 filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

async fn post_multipart(app: &Router, boundary: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/stage")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn stages_multiple_files_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let app = Router::new()
        .route("/stage", post(stage_handler))
        .with_state(StageState {
            dir: dir.path().to_path_buf(),
            max: 10 * 1024 * 1024,
        });

    let boundary = "BOUNDARY";
    let body = multipart_body(
        boundary,
        &[
            ("file", "a.bin", b"hello world"),
            ("file", "b.bin", &vec![7u8; 4096]),
        ],
    );
    let (status, value) = post_multipart(&app, boundary, body).await;
    assert_eq!(status, StatusCode::OK);
    let items = value.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["filename"], "a.bin");
    assert_eq!(items[0]["size"], 11);
    assert_eq!(items[0]["sha256"], sha256_hex(b"hello world"));
    assert_eq!(items[1]["size"], 4096);
    // The staged temp files are cleaned up once the response is returned.
    let count = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(count, 0, "temp files are removed after staging");
}

#[tokio::test]
async fn over_limit_is_413_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let app = Router::new()
        .route("/stage", post(stage_handler))
        .with_state(StageState {
            dir: dir.path().to_path_buf(),
            max: 100,
        });

    let boundary = "B";
    let body = multipart_body(boundary, &[("file", "big.bin", &vec![1u8; 5000])]);
    let (status, _) = post_multipart(&app, boundary, body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // The oversized temp file was removed.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------
// US-8 — pipeline orchestration + dedupe
// ---------------------------------------------------------------------------

fn deps<'a>(
    jobs: &'a FakeJobs,
    music: &'a FakeMusicApi,
    waveforms: &'a FakeWaveforms,
    tracks: &'a FakeTrackApi,
    tmp: &'a Path,
) -> PipelineDeps<'a> {
    PipelineDeps {
        upload_jobs: jobs,
        music,
        waveforms,
        tracks,
        ffprobe_bin: "ffprobe",
        ffmpeg_bin: "ffmpeg",
        tmp_dir: tmp,
    }
}

#[tokio::test]
async fn pipeline_master_only_for_accepted_codec() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_audio(dir.path(), "tone.wav", &[]);

    let jobs = FakeJobs::new();
    let music = FakeMusicApi::new();
    let waveforms = FakeWaveforms::new();
    let tracks = FakeTrackApi::new();
    let staged = stage_file(dir.path(), "tone.wav", "audio/wav", &wav);

    let outcome = run(
        &deps(&jobs, &music, &waveforms, &tracks, dir.path()),
        "c-1",
        staged,
    )
    .await
    .expect("pipeline");
    assert!(!outcome.deduped);
    assert!(!outcome.derivate);
    assert_eq!(music.stored_count(), 1);
    assert_eq!(music.put_count(), 1, "master only");
    assert_eq!(jobs.state_of(&outcome.job_id).as_deref(), Some("done"));
    // US-8: a local library track was registered (private, owned by the uploader).
    assert_eq!(tracks.all().len(), 1, "one local track");
    assert_eq!(tracks.all()[0].visibility, "private");
    assert_eq!(tracks.all()[0].caster_id.as_deref(), Some("c-1"));
    // D8: the pipeline computed and stored a waveform for the new track.
    assert_eq!(waveforms.count(), 1, "waveform written once");
    let wf = waveforms.last().expect("waveform");
    assert_eq!(wf.subject_kind, "track");
    assert_eq!(
        wf.subject_id,
        outcome.local_track_id.clone().expect("local track id")
    );
    assert_eq!(wf.bars, 800);
    assert_eq!(wf.data.len(), 800 * 2, "2 * bars min/max bytes");
}

#[tokio::test]
async fn pipeline_creates_derivate_for_unaccepted_codec() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // AC-3 is not an accepted master codec → derivate required.
    let ac3 = make_audio(dir.path(), "tone.ac3", &["-c:a", "ac3"]);

    let jobs = FakeJobs::new();
    let music = FakeMusicApi::new();
    let waveforms = FakeWaveforms::new();
    let tracks = FakeTrackApi::new();
    let staged = stage_file(dir.path(), "tone.ac3", "audio/ac3", &ac3);

    let outcome = run(
        &deps(&jobs, &music, &waveforms, &tracks, dir.path()),
        "c-1",
        staged,
    )
    .await
    .expect("pipeline");
    assert!(outcome.derivate, "derivate expected for ac3");
    assert_eq!(music.stored_count(), 2);
    assert_eq!(music.put_count(), 2, "master + derivate");
    // The derivate is a distinct temp file (random temp name).
    let paths = music.put_paths();
    assert_ne!(paths[0], paths[1]);
    assert_eq!(jobs.state_of(&outcome.job_id).as_deref(), Some("done"));
}

#[tokio::test]
async fn pipeline_dedupes_identical_bytes() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_audio(dir.path(), "tone.wav", &[]);
    let jobs = FakeJobs::new();
    let music = FakeMusicApi::new();
    let waveforms = FakeWaveforms::new();
    let tracks = FakeTrackApi::new();

    let first = run(
        &deps(&jobs, &music, &waveforms, &tracks, dir.path()),
        "c-1",
        stage_file(dir.path(), "tone.wav", "audio/wav", &wav),
    )
    .await
    .expect("first");
    assert!(!first.deduped);
    assert_eq!(music.stored_count(), 1);
    assert_eq!(waveforms.count(), 1);

    let second = run(
        &deps(&jobs, &music, &waveforms, &tracks, dir.path()),
        "c-1",
        stage_file(dir.path(), "tone.wav", "audio/wav", &wav),
    )
    .await
    .expect("second");
    assert!(second.deduped, "identical bytes must dedupe");
    assert_eq!(music.stored_count(), 1, "no second upload");
    assert_eq!(second.music_object_hash, first.music_object_hash);
    assert_eq!(tracks.all().len(), 1, "no second local track");
    assert_eq!(jobs.state_of(&second.job_id).as_deref(), Some("done"));
    // The dedupe short-circuit skips waveform computation (track already has one).
    assert_eq!(waveforms.count(), 1, "no second waveform on dedupe");
}

#[tokio::test]
async fn pipeline_object_api_chain_head_put_work() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_audio(dir.path(), "tone.wav", &[]);

    let jobs = FakeJobs::new();
    let music = FakeMusicApi::new();
    let waveforms = FakeWaveforms::new();
    let tracks = FakeTrackApi::new();

    let mut staged = stage_file(dir.path(), "tone.wav", "audio/wav", &wav);
    staged.isrc = Some("DEZZ00000060".into());
    let sha = staged.sha256.clone();

    let outcome = run(
        &deps(&jobs, &music, &waveforms, &tracks, dir.path()),
        "c-1",
        staged,
    )
    .await
    .expect("pipeline");

    // HEAD /objects/{sha256} is probed exactly once before the store.
    assert_eq!(music.head_requests(), vec![sha.clone()], "one HEAD probe");
    // PUT /objects/{sha256} stored the master exactly once and carried X-ISRC.
    assert_eq!(music.stored_count(), 1, "master stored");
    assert_eq!(music.put_count(), 1, "master only");
    assert_eq!(
        music.put_isrcs(),
        vec![Some("DEZZ00000060".to_string())],
        "X-ISRC forwarded on the PUT"
    );
    // POST /work {"kind":"analyze"} enqueued for the freshly stored object.
    assert_eq!(
        music.work_items(),
        vec![("analyze".to_string(), sha.clone())],
        "analyze work enqueued once"
    );
    // The local track carries the object hash as its music_api_id (US-8).
    assert_eq!(tracks.all().len(), 1);
    assert_eq!(tracks.all()[0].music_api_id, sha);
    assert_eq!(outcome.music_object_hash, sha);

    // Dedupe: an already-present object (HEAD 200) short-circuits the chain —
    // no PUT, no second /work, no second local track.
    let dedupe_music = FakeMusicApi::new();
    dedupe_music.seed_object(&sha);
    let dedupe_tracks = FakeTrackApi::new();
    let mut again = stage_file(dir.path(), "tone.wav", "audio/wav", &wav);
    again.isrc = Some("DEZZ00000060".into());
    let second = run(
        &deps(&jobs, &dedupe_music, &waveforms, &dedupe_tracks, dir.path()),
        "c-1",
        again,
    )
    .await
    .expect("second");
    assert!(second.deduped, "HEAD hit must dedupe");
    assert_eq!(dedupe_music.head_requests(), vec![sha.clone()]);
    assert_eq!(dedupe_music.put_count(), 0, "no PUT on dedupe");
    assert!(dedupe_music.work_items().is_empty(), "no /work on dedupe");
    assert_eq!(dedupe_tracks.all().len(), 1, "local track still registered");
}

#[tokio::test]
async fn pipeline_ffprobe_error_marks_job_failed() {
    if !have("ffprobe") {
        eprintln!("skipping: ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let jobs = FakeJobs::new();
    let music = FakeMusicApi::new();
    let waveforms = FakeWaveforms::new();
    let tracks = FakeTrackApi::new();
    let staged = stage_bytes(
        dir.path(),
        "garbage.bin",
        "application/octet-stream",
        b"not audio",
    );

    let err = run(
        &deps(&jobs, &music, &waveforms, &tracks, dir.path()),
        "c-1",
        staged,
    )
    .await
    .expect_err("ffprobe must fail");
    assert!(matches!(err, PipelineError { .. }));
    assert_eq!(jobs.state_of(&err.job_id).as_deref(), Some("failed"));
    let job = jobs.job_of(&err.job_id).unwrap();
    assert!(job.error.unwrap().contains("ffprobe"));
    assert_eq!(music.stored_count(), 0);
    assert_eq!(waveforms.count(), 0);
}

// ---------------------------------------------------------------------------
// US-9 — polling fragment
// ---------------------------------------------------------------------------

fn job(state: &str) -> UploadJob {
    UploadJob {
        id: "job-1".into(),
        caster_id: Some("c-1".into()),
        filename: "song.flac".into(),
        size_bytes: Some(42),
        sha256: None,
        state: state.into(),
        track_id: None,
        error: None,
        attempts: 0,
        created_at: "2026-11-18T12:00:00.000Z".into(),
        updated_at: "2026-11-18T12:00:00.000Z".into(),
    }
}

#[test]
fn pending_fragment_polls() {
    let html = UploadJobFragment {
        csrf_token: "t".into(),
        job: job("pending"),
    }
    .render()
    .unwrap();
    assert!(html.contains("hx-get=\"/library/upload/job-1\""), "{html}");
    assert!(html.contains("hx-trigger=\"every 1s\""), "{html}");
}

#[test]
fn done_fragment_does_not_poll() {
    let html = UploadJobFragment {
        csrf_token: "t".into(),
        job: job("done"),
    }
    .render()
    .unwrap();
    assert!(!html.contains("hx-trigger"), "{html}");
    assert!(html.contains("done"), "{html}");
}

#[test]
fn library_page_lists_jobs() {
    let html = LibraryPage {
        csrf_token: "t".into(),
        jobs: vec![job("failed")],
        tracks: vec![],
        q: String::new(),
        visibility: "all".into(),
        owner: String::new(),
        can_delete: false,
        page: 1,
        prev_page: None,
        next_page: None,
    }
    .render()
    .unwrap();
    assert!(html.contains("song.flac"), "{html}");
    assert!(html.contains("hx-post=\"/library/upload\""), "{html}");
}
