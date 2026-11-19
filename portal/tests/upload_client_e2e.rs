//! Cross-crate end-to-end tests: the portal's write-through clients against the
//! **real** scheduler (Issue #18, §4.2).
//!
//! `zukka_scheduler::api::router` is served on an ephemeral TCP port over a
//! migrated temporary SQLite database, and the portal's [`HttpUploadJobApi`] /
//! [`HttpWaveformApi`] talk to it. This exercises the true portal↔scheduler
//! contract: service-token auth (`401` without/with a wrong token) and the
//! `404`/`409`/`422` → [`JobError`]/[`WaveformApiError`] status mapping.

use std::sync::OnceLock;

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_portal::api_client::{
    HttpUploadJobApi, HttpWaveformApi, NewUploadJob, NewWaveform, UploadJobApi, UploadJobPatch,
    WaveformApi,
};

use zukka_scheduler::api::auth as sched_auth;
use zukka_scheduler::uuid::new_id;

const SERVICE_TOKEN: &str = "cross-crate-service-token";
const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

fn set_service_token() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: set exactly once for the whole test binary, before any request.
        unsafe { std::env::set_var(sched_auth::SERVICE_TOKEN_ENV, SERVICE_TOKEN) };
    });
}

/// A live scheduler on an ephemeral port + the migrated database behind it.
struct Scheduler {
    _dir: TempDir,
    base_url: String,
    pool: SqlitePool,
}

impl Scheduler {
    async fn boot() -> Self {
        set_service_token();
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
            .await
            .expect("connect + migrate");

        let app = zukka_scheduler::api::router(pool.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scheduler");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            _dir: dir,
            base_url: format!("http://{addr}"),
            pool,
        }
    }

    /// Seeds a caster (needed because `upload_jobs.caster_id` is a FK).
    async fn seed_caster(&self) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
             created_at, updated_at) VALUES (?, 'x', 'X', 'h', ?, 1, ?, ?)",
        )
        .bind(&id)
        .bind(ROLE_CASTER)
        .bind(TS)
        .bind(TS)
        .execute(&self.pool)
        .await
        .expect("insert caster");
        id
    }

    /// Seeds a track (needed to set `upload_jobs.track_id`).
    async fn seed_track(&self, caster: &str) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO tracks (id, music_api_id, title, artist, duration_ms, storage_key, \
             visibility, caster_id, created_at, updated_at) \
             VALUES (?, ?, 'Song', 'Artist', 1000, 'k', 'shared', ?, ?, ?)",
        )
        .bind(&id)
        .bind(&id)
        .bind(caster)
        .bind(TS)
        .bind(TS)
        .execute(&self.pool)
        .await
        .expect("insert track");
        id
    }

    fn jobs(&self) -> HttpUploadJobApi {
        HttpUploadJobApi::new(self.base_url.clone(), SERVICE_TOKEN).expect("job client")
    }

    fn waveforms(&self) -> HttpWaveformApi {
        HttpWaveformApi::new(self.base_url.clone(), SERVICE_TOKEN).expect("waveform client")
    }
}

fn new_job(caster: &str) -> NewUploadJob {
    NewUploadJob {
        caster_id: caster.to_string(),
        filename: "song.flac".to_string(),
        size_bytes: Some(1024),
        sha256: None,
    }
}

// ---------------------------------------------------------------------------
// /internal/upload-jobs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_job_client_roundtrip_and_attempts() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let track = s.seed_track(&caster).await;
    let jobs = s.jobs();

    let id = jobs.create_job(&new_job(&caster)).await.expect("create");
    assert_eq!(id.len(), 36, "UUIDv7 text PK");

    let pending = jobs.get_job(&id).await.expect("get");
    assert_eq!(pending.state, "pending");
    assert_eq!(pending.attempts, 0);

    let running = jobs
        .patch_job(
            &id,
            &UploadJobPatch {
                state: "running".into(),
                sha256: Some("deadbeef".into()),
                track_id: None,
                error: None,
            },
        )
        .await
        .expect("running");
    assert_eq!(running.state, "running");
    assert_eq!(running.attempts, 1, "entering running increments attempts");

    let done = jobs
        .patch_job(
            &id,
            &UploadJobPatch {
                state: "done".into(),
                sha256: None,
                track_id: Some(track.clone()),
                error: None,
            },
        )
        .await
        .expect("done");
    assert_eq!(done.state, "done");
    assert_eq!(done.track_id.as_deref(), Some(track.as_str()));
}

#[tokio::test]
async fn upload_job_client_maps_404_409_422() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let jobs = s.jobs();

    // 404 → JobError::not_found.
    let err = jobs
        .get_job("0192aaaa-0000-7000-8000-000000000000")
        .await
        .expect_err("unknown job");
    assert!(err.is_not_found(), "404 maps to not_found: {err}");

    // 422 → validation (empty filename).
    let err = jobs
        .create_job(&NewUploadJob {
            caster_id: caster.clone(),
            filename: "   ".into(),
            size_bytes: None,
            sha256: None,
        })
        .await
        .expect_err("blank filename");
    assert!(err.to_string().contains("filename"), "422 message: {err}");

    // 409 → conflict on an illegal transition (done -> running).
    let id = jobs.create_job(&new_job(&caster)).await.unwrap();
    jobs.patch_job(
        &id,
        &UploadJobPatch {
            state: "running".into(),
            sha256: None,
            track_id: None,
            error: None,
        },
    )
    .await
    .unwrap();
    let track = s.seed_track(&caster).await;
    jobs.patch_job(
        &id,
        &UploadJobPatch {
            state: "done".into(),
            sha256: None,
            track_id: Some(track),
            error: None,
        },
    )
    .await
    .unwrap();
    let err = jobs
        .patch_job(
            &id,
            &UploadJobPatch {
                state: "running".into(),
                sha256: None,
                track_id: None,
                error: None,
            },
        )
        .await
        .expect_err("terminal job");
    assert!(!err.is_not_found());
    assert!(err.to_string().contains("conflict"), "409 message: {err}");
}

#[tokio::test]
async fn upload_job_client_without_service_token_is_unauthorized() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let bad = HttpUploadJobApi::new(s.base_url.clone(), "wrong-token").expect("client");

    let err = bad.create_job(&new_job(&caster)).await.expect_err("401");
    assert!(err.to_string().contains("401"), "401 mapping: {err}");
}

#[tokio::test]
async fn upload_job_client_lists_newest_first() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let jobs = s.jobs();

    for name in ["a.mp3", "b.mp3", "c.mp3"] {
        jobs.create_job(&NewUploadJob {
            caster_id: caster.clone(),
            filename: name.into(),
            size_bytes: None,
            sha256: None,
        })
        .await
        .unwrap();
    }
    let listed = jobs.list_jobs(2).await.expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].filename, "c.mp3");
    assert_eq!(listed[1].filename, "b.mp3");
}

// ---------------------------------------------------------------------------
// /internal/waveforms
// ---------------------------------------------------------------------------

fn waveform(subject_id: &str) -> NewWaveform {
    NewWaveform {
        subject_kind: "track".into(),
        subject_id: subject_id.into(),
        scheme: 1,
        algorithm: "ffmpeg-minmax-8k/1".into(),
        ms_per_bar: 500,
        bars: 2,
        data: vec![128, 144, 64, 176],
    }
}

#[tokio::test]
async fn waveform_client_roundtrip_and_404() {
    let s = Scheduler::boot().await;
    let wf = s.waveforms();

    // Unknown subject → 404 → Ok(None).
    assert!(wf.get_waveform("track", "ghost").await.unwrap().is_none());

    let id = new_id();
    let put = wf.put_waveform(&waveform(&id)).await.expect("put");
    assert_eq!(put.subject_id, id);
    assert_eq!(put.bars, 2);

    let got = wf
        .get_waveform("track", &id)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(got.data, vec![128, 144, 64, 176]);
}

#[tokio::test]
async fn waveform_client_maps_422_and_401() {
    let s = Scheduler::boot().await;
    let wf = s.waveforms();

    // Invalid subject_kind → 422.
    let mut bad = waveform("t");
    bad.subject_kind = "album".into();
    let err = wf.put_waveform(&bad).await.expect_err("422");
    assert!(err.to_string().contains("422"), "422 mapping: {err}");

    // Wrong service token → 401.
    let bad_client = HttpWaveformApi::new(s.base_url.clone(), "nope").expect("client");
    assert!(bad_client.get_waveform("track", "t").await.is_err());
}
