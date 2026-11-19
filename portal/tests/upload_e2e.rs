//! Full-router end-to-end tests for Issue #18 "Upload" (portal.md §7).
//!
//! Unlike `upload_pipeline.rs` (which calls `run()` directly) and
//! `waveform_route.rs` (which injects an [`AuthContext`]), these tests drive the
//! **real** portal router from `build_router`: login (session cookie + CSRF),
//! `POST /library/upload` (multipart), the pipeline, and the library/waveform
//! routes. The Music API, the scheduler job API and the waveform API are
//! in-memory fakes; `ffprobe`/`ffmpeg` are real (tests skip when missing).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::routing::post;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use zukka_portal::api_client::{
    CasterDto, CreatedSessionDto, IdentityApi, IdentityError, JobError, NewUploadJob, NewWaveform,
    SessionValidationDto, UploadJob, UploadJobApi, UploadJobPatch, Waveform, WaveformApi,
    WaveformApiError,
};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::error::PortalError;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;
use zukka_portal::upload::stage_multipart;

// ---------------------------------------------------------------------------
// Fake identity (login + permission map control)
// ---------------------------------------------------------------------------

struct FakeCaster {
    dto: CasterDto,
    password: String,
}

struct FakeIdentity {
    casters: Mutex<Vec<FakeCaster>>,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    /// A single `caster` account (all caster permissions).
    fn caster() -> Self {
        Self::with_permissions(rbac::permissions_for_role("caster"))
    }

    /// A single account whose permission map is the given one.
    fn with_permissions(permissions: BTreeMap<String, bool>) -> Self {
        let dto = CasterDto {
            id: "c-1".into(),
            username: "mira".into(),
            display_name: "Mira".into(),
            email: None,
            role: "caster".into(),
            is_active: true,
            harbor_mount: None,
            stream_username: None,
            permissions,
        };
        Self {
            casters: Mutex::new(vec![FakeCaster {
                dto,
                password: "pw".into(),
            }]),
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.username == username && c.password == password)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::InvalidCredentials)
    }

    async fn create_session(
        &self,
        caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        let caster = self
            .casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.id == caster_id)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::NotFound)?;
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        let session_id = format!("sess-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id,
                caster,
                csrf_token: format!("csrf-{n}"),
                idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
                absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
            },
        );
        Ok(CreatedSessionDto {
            id: format!("sess-{n}"),
            token,
            csrf_token: format!("csrf-{n}"),
            idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
            absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
        })
    }

    async fn validate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError> {
        Ok(self.sessions.lock().unwrap().get(token).cloned())
    }

    async fn revoke_session(&self, _session_id: &str) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.id == caster_id)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::NotFound)
    }

    async fn change_password(
        &self,
        _caster_id: &str,
        _password: &str,
        _keep: Option<&str>,
    ) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn change_stream_password(
        &self,
        _caster_id: &str,
        _password: Option<&str>,
    ) -> Result<Option<String>, IdentityError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Fake scheduler job API
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeJobsInner {
    jobs: Vec<UploadJob>,
    counter: u64,
}

/// In-memory [`UploadJobApi`] recording every job and state transition.
#[derive(Default)]
struct FakeJobs {
    inner: Mutex<FakeJobsInner>,
}

impl FakeJobs {
    fn new() -> Self {
        Self::default()
    }

    fn all(&self) -> Vec<UploadJob> {
        self.inner.lock().unwrap().jobs.clone()
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
        self.inner
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .ok_or_else(JobError::not_found)
    }

    async fn list_jobs(&self, _limit: i64) -> Result<Vec<UploadJob>, JobError> {
        Ok(self.all())
    }
}

// ---------------------------------------------------------------------------
// Fake waveform API
// ---------------------------------------------------------------------------

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
// Harness
// ---------------------------------------------------------------------------

struct Services {
    music: Arc<FakeMusicApi>,
    jobs: Arc<FakeJobs>,
    waveforms: Arc<FakeWaveforms>,
}

fn config(tmp: &Path, max_bytes: i64) -> Config {
    let mut cfg = Config::new(
        "127.0.0.1:8081".into(),
        "https://portal.example.org".into(),
        "http://scheduler:8080".into(),
        "svc-token".into(),
        7,
        30,
    )
    .unwrap();
    cfg.upload_max_bytes = max_bytes;
    cfg.upload_tmp_dir = tmp.join("uploads");
    cfg
}

fn router(identity: FakeIdentity, cfg: Config) -> (Router, Services) {
    let music = Arc::new(FakeMusicApi::new());
    let jobs = Arc::new(FakeJobs::new());
    let waveforms = Arc::new(FakeWaveforms::new());
    let state = AppState::with_services(
        cfg,
        Arc::new(identity),
        Arc::new(WindowLimiter::new(5, Duration::from_secs(900))),
        music.clone(),
        jobs.clone(),
        waveforms.clone(),
    );
    (
        build_router(state),
        Services {
            music,
            jobs,
            waveforms,
        },
    )
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(bytes) => builder.body(Body::from(bytes)).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

fn set_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        let first = raw.split(';').next().unwrap_or("");
        if let Some(rest) = first.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn meta_csrf(body: &str) -> Option<String> {
    let marker = "name=\"csrf-token\" content=\"";
    let start = body.find(marker)? + marker.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Logs in through the real portal; returns `(session_cookie, csrf_token)`.
async fn login(app: &Router) -> (String, String) {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let cookie = set_cookie_value(&headers, "__Host-zukka_csrf").expect("csrf cookie");
    let token = meta_csrf(&body).expect("csrf meta");
    let payload = form(&[
        ("username", "mira"),
        ("password", "pw"),
        ("csrf_token", &token),
    ]);
    let (status, headers, _) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={cookie}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(payload.into_bytes()),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login");
    let session = set_cookie_value(&headers, "__Host-zukka_session").expect("session cookie");
    let (_, _, dashboard) = send(
        app,
        Method::GET,
        "/",
        &[("cookie", &format!("__Host-zukka_session={session}"))],
        None,
    )
    .await;
    let csrf = meta_csrf(&dashboard).expect("dashboard csrf");
    (session, csrf)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_wav(dir: &Path, name: &str) -> std::path::PathBuf {
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
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg generate {name}");
    out
}

/// An `mp4` carrying a video **and** an audio stream (D7 condition b).
fn make_video(dir: &Path, name: &str) -> std::path::PathBuf {
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
            "testsrc=duration=0.5:size=64x64:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.5",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
        ])
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

/// A raw `multipart/form-data` body with a single file part.
fn multipart_body(boundary: &str, filename: &str, content_type: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

async fn upload(
    app: &Router,
    session: &str,
    csrf: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> StatusCode {
    let boundary = "ZUKKA-BOUNDARY";
    let body = multipart_body(boundary, filename, content_type, data);
    let (status, _, _) = send(
        app,
        Method::POST,
        "/library/upload",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("x-csrf-token", csrf),
            ("sec-fetch-site", "same-origin"),
            (
                "content-type",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
        ],
        Some(body),
    )
    .await;
    status
}

// ---------------------------------------------------------------------------
// Multipart → temp → ffprobe → Music API → upload_jobs (end-to-end)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_accepted_codec_end_to_end_via_router() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_wav(dir.path(), "tone.wav");
    let bytes = std::fs::read(&wav).unwrap();

    let (app, svc) = router(
        FakeIdentity::caster(),
        config(dir.path(), 500 * 1024 * 1024),
    );
    let (session, csrf) = login(&app).await;

    let status = upload(&app, &session, &csrf, "tone.wav", "audio/wav", &bytes).await;
    assert_eq!(status, StatusCode::OK, "upload accepted");

    // Music API: one track, master asset only (pcm_s16le is an accepted codec).
    assert_eq!(svc.music.created_count(), 1, "one track created");
    assert_eq!(svc.music.asset_count(), 1, "master only, no derivate");
    // Job reached `done` with the track id and the streamed SHA-256.
    let jobs = svc.jobs.all();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, "done");
    assert_eq!(jobs[0].sha256.as_deref(), Some(sha256_hex(&bytes).as_str()));
    assert!(jobs[0].track_id.is_some(), "job carries the track id");
    // D8: the pipeline computed and stored exactly one waveform for the track.
    assert_eq!(svc.waveforms.count(), 1);

    // The upload temp dir is empty again (staging + derivate temps dropped).
    assert_eq!(
        std::fs::read_dir(dir.path().join("uploads"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn video_track_gets_derivate_master_kept() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mp4 = make_video(dir.path(), "clip.mp4");
    let bytes = std::fs::read(&mp4).unwrap();

    let (app, svc) = router(
        FakeIdentity::caster(),
        config(dir.path(), 500 * 1024 * 1024),
    );
    let (session, csrf) = login(&app).await;

    let status = upload(&app, &session, &csrf, "clip.mp4", "video/mp4", &bytes).await;
    assert_eq!(status, StatusCode::OK);

    // D7: a video stream forces a derivate → master + derivate, one track.
    assert_eq!(svc.music.created_count(), 1);
    assert_eq!(svc.music.asset_count(), 2, "master + MP3 derivate");
    let paths = svc.music.asset_paths();
    assert_ne!(paths[0], paths[1], "master and derivate are distinct files");
    let job = &svc.jobs.all()[0];
    assert_eq!(job.state, "done");
}

#[tokio::test]
async fn duplicate_upload_creates_no_second_track() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_wav(dir.path(), "tone.wav");
    let bytes = std::fs::read(&wav).unwrap();

    let (app, svc) = router(
        FakeIdentity::caster(),
        config(dir.path(), 500 * 1024 * 1024),
    );
    let (session, csrf) = login(&app).await;

    assert_eq!(
        upload(&app, &session, &csrf, "tone.wav", "audio/wav", &bytes).await,
        StatusCode::OK
    );
    assert_eq!(
        upload(&app, &session, &csrf, "tone.wav", "audio/wav", &bytes).await,
        StatusCode::OK
    );

    // Hash-Dedupe: the second upload short-circuits to the existing track.
    assert_eq!(svc.music.created_count(), 1, "no second track");
    assert_eq!(svc.waveforms.count(), 1, "no second waveform");
    let jobs = svc.jobs.all();
    assert_eq!(jobs.len(), 2, "both uploads are tracked as jobs");
    assert!(jobs.iter().all(|j| j.state == "done"));
    assert_eq!(jobs[0].track_id, jobs[1].track_id, "same track id");
}

// ---------------------------------------------------------------------------
// Waveform (D8) routes through the real router
// ---------------------------------------------------------------------------

#[tokio::test]
async fn waveform_json_route_200_and_404() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_wav(dir.path(), "tone.wav");
    let bytes = std::fs::read(&wav).unwrap();

    let (app, svc) = router(
        FakeIdentity::caster(),
        config(dir.path(), 500 * 1024 * 1024),
    );
    let (session, csrf) = login(&app).await;
    assert_eq!(
        upload(&app, &session, &csrf, "tone.wav", "audio/wav", &bytes).await,
        StatusCode::OK
    );
    let track = svc.jobs.all()[0].track_id.clone().unwrap();
    let cookie = format!("__Host-zukka_session={session}");

    let (status, _, body) = send(
        &app,
        Method::GET,
        &format!("/library/tracks/{track}/waveform.json"),
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["subject_kind"], "track");
    assert_eq!(value["bars"], 800);
    assert_eq!(value["data"].as_array().unwrap().len(), 800 * 2);

    // Unknown track → 404.
    let (status, _, _) = send(
        &app,
        Method::GET,
        "/library/tracks/does-not-exist/waveform.json",
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 413 + temp cleanup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn over_max_bytes_is_413_and_cleans_temp() {
    let dir = tempfile::tempdir().unwrap();
    // Cap far below the body so the handler's own limit (not the framework's)
    // is what rejects the upload.
    let (app, svc) = router(FakeIdentity::caster(), config(dir.path(), 1000));
    let (session, csrf) = login(&app).await;

    let status = upload(
        &app,
        &session,
        &csrf,
        "big.bin",
        "application/octet-stream",
        &vec![9u8; 5000],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // No job/track was created and the temp dir holds nothing.
    assert_eq!(svc.jobs.all().len(), 0);
    assert_eq!(svc.music.created_count(), 0);
    let uploads = dir.path().join("uploads");
    let leftover = std::fs::read_dir(&uploads).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftover, 0, "oversized temp file was removed");
}

// ---------------------------------------------------------------------------
// RBAC / CSRF
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_without_permission_is_403() {
    let dir = tempfile::tempdir().unwrap();
    // A caster whose permission map lacks `library.upload`.
    let mut perms = rbac::permissions_for_role("caster");
    perms.insert("library.upload".into(), false);
    let (app, svc) = router(
        FakeIdentity::with_permissions(perms),
        config(dir.path(), 1_000_000),
    );
    let (session, csrf) = login(&app).await;

    let status = upload(&app, &session, &csrf, "tone.wav", "audio/wav", b"data").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(svc.music.created_count(), 0, "nothing was created");
    assert_eq!(svc.jobs.all().len(), 0);
}

#[tokio::test]
async fn upload_without_csrf_token_is_403() {
    let dir = tempfile::tempdir().unwrap();
    let (app, svc) = router(FakeIdentity::caster(), config(dir.path(), 1_000_000));
    let (session, _csrf) = login(&app).await;

    let boundary = "B";
    let body = multipart_body(boundary, "tone.wav", "audio/wav", b"data");
    // Valid session but no CSRF token → 403 (the CSRF layer runs first).
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/library/upload",
        &[
            ("cookie", &format!("__Host-zukka_session={session}")),
            ("sec-fetch-site", "same-origin"),
            (
                "content-type",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
        ],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(svc.jobs.all().len(), 0);
}

// ---------------------------------------------------------------------------
// Streaming multipart (no full RAM buffer)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct StageState {
    dir: std::path::PathBuf,
    max: i64,
}

async fn stage_handler(
    State(state): State<StageState>,
    mut multipart: Multipart,
) -> Result<axum::Json<Value>, PortalError> {
    let staged = stage_multipart(&mut multipart, &state.dir, state.max).await?;
    let out: Vec<Value> = staged
        .iter()
        .map(|s| json!({"filename": s.filename, "size": s.size_bytes, "sha256": s.sha256}))
        .collect();
    Ok(axum::Json(json!(out)))
}

fn chunk_byte(i: usize) -> u8 {
    (i % 251) as u8
}

/// Builds a `Body::from_stream` of `chunks` × `chunk_len` bytes plus the
/// multipart envelope, and the expected `(total_size, sha256)` of the file
/// content — both without ever materialising the whole body in RAM.
fn streamed_body(
    boundary: &str,
    filename: &str,
    chunks: usize,
    chunk_len: usize,
) -> (Body, u64, String) {
    let preamble = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
         filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    let epilogue = format!("\r\n--{boundary}--\r\n").into_bytes();

    let head =
        futures_util::stream::once(
            async move { Ok::<Bytes, std::io::Error>(Bytes::from(preamble)) },
        );
    let mid = futures_util::stream::iter((0..chunks).map(move |i| {
        let mut buf = vec![0u8; chunk_len];
        buf.fill(chunk_byte(i));
        Ok::<Bytes, std::io::Error>(Bytes::from(buf))
    }));
    let tail =
        futures_util::stream::once(
            async move { Ok::<Bytes, std::io::Error>(Bytes::from(epilogue)) },
        );
    let body = Body::from_stream(head.chain(mid).chain(tail));

    // Expected size + hash, computed streaming (one chunk buffer at a time).
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; chunk_len];
    for i in 0..chunks {
        buf.fill(chunk_byte(i));
        hasher.update(&buf);
    }
    let sha = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    (body, (chunks * chunk_len) as u64, sha)
}

async fn post_streamed(app: &Router, boundary: &str, body: Body) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/stage")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn streaming_chunked_body_stages_and_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let app = Router::new()
        .route("/stage", post(stage_handler))
        .with_state(StageState {
            dir: dir.path().to_path_buf(),
            max: 100 * 1024 * 1024,
        });

    // ~1.5 MiB delivered as 24 × 64 KiB chunks (streamed, not buffered).
    let (body, size, sha) = streamed_body("B", "stream.bin", 24, 64 * 1024);
    let (status, value) = post_streamed(&app, "B", body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let item = &value.as_array().unwrap()[0];
    assert_eq!(item["size"].as_u64(), Some(size));
    assert_eq!(item["sha256"].as_str(), Some(sha.as_str()));
    // Temps are cleaned up once the response is returned.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// AC §7 "200-MB-FLAC-Upload läuft ohne RAM-Spitze (Streaming)".
///
/// BLOCKED (→ issue #58): axum's `Multipart` extractor applies the default
/// `DefaultBodyLimit` of 2 MiB (it calls `with_limited_body`), and the portal
/// never disables it, so any body > 2 MiB is rejected before staging. This test
/// documents the intended behaviour; it must be un-ignored once
/// `DefaultBodyLimit::disable()` (or a `UPLOAD_MAX_BYTES`-sized limit) is wired
/// into the upload route.
#[tokio::test]
#[ignore = "blocked by #58: portal never disables axum's default 2 MiB Multipart body limit"]
async fn large_streamed_upload_stages_without_body_limit() {
    let dir = tempfile::tempdir().unwrap();
    let app = Router::new()
        .route("/stage", post(stage_handler))
        .with_state(StageState {
            dir: dir.path().to_path_buf(),
            max: 500 * 1024 * 1024,
        });

    // 16 MiB streamed in 64 KiB chunks (> the 2 MiB framework default).
    let (body, size, sha) = streamed_body("B", "big.flac", 256, 64 * 1024);
    let (status, value) = post_streamed(&app, "B", body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let item = &value.as_array().unwrap()[0];
    assert_eq!(item["size"].as_u64(), Some(size));
    assert_eq!(item["sha256"].as_str(), Some(sha.as_str()));
}
