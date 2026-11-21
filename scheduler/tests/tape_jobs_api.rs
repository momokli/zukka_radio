//! Store + HTTP tests for the tape-job queue (Issue #25, M3).
//!
//! The scheduler is the only writer; these tests exercise claim atomicity,
//! backoff, state transitions and the `tapes` side effects through both the
//! store and the service-token-guarded `/internal/tape-jobs/*` routes.

use std::sync::OnceLock;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::api::auth as api_auth;
use zukka_scheduler::dto::{PatchTapeJobRequest, UpsertWaveformRequest};
use zukka_scheduler::store::tape_jobs;
use zukka_scheduler::store::waveforms;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

fn service_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "tape-jobs-test-token".to_string();
        // SAFETY: set once for the whole test binary, before any request.
        unsafe { std::env::set_var(api_auth::SERVICE_TOKEN_ENV, &token) };
        token
    })
}

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    (dir, pool)
}

async fn seed_tape(pool: &SqlitePool, id: &str, slug: &str, status: &str) {
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES ('c-tape', 'tapeuser', 'Tape User', 'x', ?, 1, ?, ?)",
    )
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");

    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, storage_key, \
         status, visible, created_at, updated_at) \
         VALUES (?, ?, 'c-tape', '2026-11-23T10:00:00.000Z', '2026-11-23T12:00:00.000Z', \
         'live', 'recordings/a.mp3', ?, 1, ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(status)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");
}

async fn seed_job(
    pool: &SqlitePool,
    id: &str,
    tape_id: &str,
    kind: &str,
    state: &str,
    attempts: i64,
    updated_at: &str,
) {
    sqlx::query(
        "INSERT INTO tape_jobs (id, tape_id, kind, state, attempts, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(tape_id)
    .bind(kind)
    .bind(state)
    .bind(attempts)
    .bind(TS)
    .bind(updated_at)
    .execute(pool)
    .await
    .expect("insert tape job");
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    with_token: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if with_token {
        builder = builder.header("authorization", format!("Bearer {}", service_token()));
    }
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_increments_attempts_and_is_disjoint() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "pending").await;
    seed_job(&pool, "j1", "t1", "probe", "pending", 0, TS).await;

    let first = tape_jobs::claim(&pool, &["probe".into()], 1, 600)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempts, 1);
    assert_eq!(first[0].slug, "slug1");
    assert_eq!(first[0].storage_key.as_deref(), Some("recordings/a.mp3"));

    // The job is now running → a second claim gets nothing.
    let second = tape_jobs::claim(&pool, &["probe".into()], 1, 600)
        .await
        .unwrap();
    assert!(second.is_empty());
}

#[tokio::test]
async fn concurrent_claims_do_not_overlap() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "pending").await;
    for index in 0..2 {
        seed_job(
            &pool,
            &format!("j{index}"),
            "t1",
            if index == 0 { "probe" } else { "peaks" },
            "pending",
            0,
            TS,
        )
        .await;
    }

    let kinds = vec!["probe".to_string(), "peaks".to_string()];
    let (a, b) = tokio::join!(
        tape_jobs::claim(&pool, &kinds, 1, 600),
        tape_jobs::claim(&pool, &kinds, 1, 600)
    );
    let mut ids: Vec<String> = a
        .unwrap()
        .into_iter()
        .chain(b.unwrap())
        .map(|job| job.id)
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2, "each claim must get a distinct job");
}

#[tokio::test]
async fn backoff_parks_a_retried_job_until_its_due_time() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "processing").await;
    // attempts=2 → 10 s backoff. Updated "now" → not due yet.
    seed_job(&pool, "j1", "t1", "probe", "pending", 2, TS).await;
    let just_retried = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    sqlx::query("UPDATE tape_jobs SET updated_at = ? WHERE id = 'j1'")
        .bind(&just_retried)
        .execute(&pool)
        .await
        .unwrap();

    let claimed = tape_jobs::claim(&pool, &["probe".into()], 1, 600)
        .await
        .unwrap();
    assert!(claimed.is_empty(), "backoff window must hide the job");

    // Rewind past the 10 s window.
    let old = (chrono::Utc::now() - chrono::Duration::seconds(11))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    sqlx::query("UPDATE tape_jobs SET updated_at = ? WHERE id = 'j1'")
        .bind(&old)
        .execute(&pool)
        .await
        .unwrap();
    let claimed = tape_jobs::claim(&pool, &["probe".into()], 1, 600)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 3);
}

#[tokio::test]
async fn lease_recovery_requeues_stale_running_jobs() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "processing").await;
    let long_ago = (chrono::Utc::now() - chrono::Duration::seconds(3600))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    seed_job(&pool, "j1", "t1", "probe", "running", 1, &long_ago).await;

    let claimed = tape_jobs::claim(&pool, &["probe".into()], 1, 600)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 2);
}

#[tokio::test]
async fn probe_done_updates_tape_and_peaks_done_marks_ready() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "pending").await;
    seed_job(&pool, "jp", "t1", "probe", "running", 1, TS).await;
    seed_job(&pool, "jk", "t1", "peaks", "running", 1, TS).await;

    tape_jobs::patch(
        &pool,
        "jp",
        &PatchTapeJobRequest {
            state: "done".into(),
            duration_ms: Some(7_194_000),
            format: Some("mp3".into()),
            mime: Some("audio/mpeg".into()),
            error: None,
        },
    )
    .await
    .unwrap();
    let tape: (Option<i64>, Option<String>, String) =
        sqlx::query_as("SELECT duration_ms, format, status FROM tapes WHERE id = 't1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tape.0, Some(7_194_000));
    assert_eq!(tape.1.as_deref(), Some("mp3"));
    assert_eq!(tape.2, "processing");

    tape_jobs::patch(
        &pool,
        "jk",
        &PatchTapeJobRequest {
            state: "done".into(),
            duration_ms: None,
            format: None,
            mime: None,
            error: None,
        },
    )
    .await
    .unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM tapes WHERE id = 't1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "ready");
}

#[tokio::test]
async fn failed_patch_sets_tape_failed_and_error() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "processing").await;
    seed_job(&pool, "j1", "t1", "probe", "running", 1, TS).await;

    let dto = tape_jobs::patch(
        &pool,
        "j1",
        &PatchTapeJobRequest {
            state: "failed".into(),
            duration_ms: None,
            format: None,
            mime: None,
            error: Some("ffmpeg exit 1".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(dto.state, "failed");
    assert_eq!(dto.error.as_deref(), Some("ffmpeg exit 1"));
    let status: String = sqlx::query_scalar("SELECT status FROM tapes WHERE id = 't1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
}

#[tokio::test]
async fn done_job_is_terminal() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "processing").await;
    seed_job(&pool, "j1", "t1", "probe", "running", 1, TS).await;
    tape_jobs::patch(
        &pool,
        "j1",
        &PatchTapeJobRequest {
            state: "done".into(),
            duration_ms: Some(1000),
            format: None,
            mime: None,
            error: None,
        },
    )
    .await
    .unwrap();

    let err = tape_jobs::patch(
        &pool,
        "j1",
        &PatchTapeJobRequest {
            state: "failed".into(),
            duration_ms: None,
            format: None,
            mime: None,
            error: Some("late".into()),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        zukka_scheduler::error::ApiError::Conflict { .. }
    ));
}

#[tokio::test]
async fn tape_waveform_length_follows_kind() {
    let (_dir, pool) = setup().await;
    let mut tape = UpsertWaveformRequest {
        subject_kind: "tape".into(),
        subject_id: "t1".into(),
        scheme: 1,
        algorithm: "ffmpeg-rms-500ms/1".into(),
        ms_per_bar: 500,
        bars: 3,
        data: vec![0, 128, 255],
    };
    assert!(waveforms::upsert(&pool, &tape).await.is_ok());
    tape.data = vec![0, 128, 255, 1, 2, 3];
    assert!(waveforms::upsert(&pool, &tape).await.is_err());

    let track = UpsertWaveformRequest {
        subject_kind: "track".into(),
        subject_id: "tr1".into(),
        scheme: 1,
        algorithm: "ffmpeg-minmax-8k/1".into(),
        ms_per_bar: 500,
        bars: 3,
        data: vec![0; 6],
    };
    assert!(waveforms::upsert(&pool, &track).await.is_ok());
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn internal_routes_require_service_token() {
    let (_dir, pool) = setup().await;
    let app = app(pool);
    for (method, uri) in [
        (Method::POST, "/api/v1/internal/tape-jobs/claim"),
        (Method::PATCH, "/api/v1/internal/tape-jobs/whatever"),
    ] {
        let (status, _) = send(&app, method, uri, Some(json!({})), false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn claim_endpoint_returns_jobs_and_validates_input() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "pending").await;
    seed_job(&pool, "j1", "t1", "probe", "pending", 0, TS).await;
    let app = app(pool);

    let (status, value) = send(
        &app,
        Method::POST,
        "/api/v1/internal/tape-jobs/claim",
        Some(json!({ "kinds": ["probe"], "limit": 4 })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(value["jobs"][0]["attempts"], 1);

    for body in [
        json!({ "kinds": ["nope"] }),
        json!({ "limit": 0 }),
        json!({ "limit": 17 }),
    ] {
        let (status, value) = send(
            &app,
            Method::POST,
            "/api/v1/internal/tape-jobs/claim",
            Some(body),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(value["error"]["code"], "validation_error");
    }
}

#[tokio::test]
async fn patch_endpoint_enforces_probe_duration() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "pending").await;
    seed_job(&pool, "j1", "t1", "probe", "running", 1, TS).await;
    let app = app(pool);

    let (status, value) = send(
        &app,
        Method::PATCH,
        "/api/v1/internal/tape-jobs/j1",
        Some(json!({ "state": "done" })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value["error"]["details"]["field"], "duration_ms");

    let (status, value) = send(
        &app,
        Method::PATCH,
        "/api/v1/internal/tape-jobs/j1",
        Some(json!({
            "state": "done",
            "duration_ms": 7194000,
            "format": "mp3",
            "mime": "audio/mpeg"
        })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["state"], "done");
    assert_eq!(value["attempts"], 1);
}

#[tokio::test]
async fn ingest_jobs_are_not_worker_claimable_or_patchable() {
    let (_dir, pool) = setup().await;
    seed_tape(&pool, "t1", "slug1", "pending").await;
    seed_job(&pool, "ju", "t1", "upload", "pending", 0, TS).await;
    seed_job(&pool, "jp", "t1", "probe", "pending", 0, TS).await;
    let app = app(pool);

    // #132: the worker must not be able to claim the ingest job.
    let (status, value) = send(
        &app,
        Method::POST,
        "/api/v1/internal/tape-jobs/claim",
        Some(json!({ "kinds": ["upload"] })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], "bad_request");

    // The default claim still returns only probe/peaks jobs.
    let (status, value) = send(
        &app,
        Method::POST,
        "/api/v1/internal/tape-jobs/claim",
        Some(json!({ "limit": 4 })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs = value["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["kind"], "probe");

    // PATCH on an ingest job is refused with 409.
    let (status, value) = send(
        &app,
        Method::PATCH,
        "/api/v1/internal/tape-jobs/ju",
        Some(json!({ "state": "failed", "error": "nope" })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"], "upload_job_not_patchable");
}

#[tokio::test]
async fn tape_waveform_http_accepts_bars_bytes_and_rejects_pairs() {
    let (_dir, pool) = setup().await;
    let app = app(pool);
    let base = json!({
        "subject_kind": "tape",
        "subject_id": "t1",
        "scheme": 1,
        "algorithm": "ffmpeg-rms-500ms/1",
        "ms_per_bar": 500,
        "bars": 2,
    });

    let mut body = base.clone();
    body["data"] = json!([0, 255]);
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/internal/waveforms",
        Some(body),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let mut bad = base;
    bad["data"] = json!([0, 0, 255, 255]);
    let (status, value) = send(
        &app,
        Method::POST,
        "/api/v1/internal/waveforms",
        Some(bad),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value["error"]["details"]["field"], "data");
}
