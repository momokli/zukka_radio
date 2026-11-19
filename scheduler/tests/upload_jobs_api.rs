//! Store + HTTP tests for upload jobs (Issue #18: US-1, US-2, US-10).
//!
//! Store tests run `zukka_scheduler::store::upload_jobs` directly against a
//! migrated temporary SQLite database. API tests go through
//! `zukka_scheduler::api::router` with `tower::ServiceExt::oneshot`; the
//! `/internal/*` routes require the service token (§4.1).

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
use zukka_scheduler::dto::{CreateUploadJobRequest, PatchUploadJobRequest};
use zukka_scheduler::recovery;
use zukka_scheduler::store::upload_jobs;
use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

fn service_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let token = "upload-test-token".to_string();
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

async fn seed_caster(pool: &SqlitePool) -> String {
    let id = new_id();
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES (?, 'uploader', 'Uploader', 'x', ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id
}

/// Seeds a `tracks` row so `upload_jobs.track_id` (FK) can be set.
async fn seed_track(pool: &SqlitePool, caster: &str) -> String {
    let id = new_id();
    sqlx::query(
        "INSERT INTO tracks (id, music_api_id, title, artist, duration_ms, storage_key, \
         visibility, caster_id, created_at, updated_at) \
         VALUES (?, ?, 'Song', 'Artist', 1000, 'key', 'shared', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&id)
    .bind(caster)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert track");
    id
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
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn new_job(caster: &str) -> CreateUploadJobRequest {
    CreateUploadJobRequest {
        caster_id: caster.to_string(),
        filename: "song.flac".to_string(),
        size_bytes: Some(1024),
        sha256: None,
    }
}

// ---------------------------------------------------------------------------
// US-1 — store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_get_and_initial_state() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;

    let job = upload_jobs::create(&pool, &new_job(&caster))
        .await
        .expect("create");
    assert_eq!(job.state, "pending");
    assert_eq!(job.attempts, 0);
    assert_eq!(job.filename, "song.flac");
    assert_eq!(job.id.len(), 36);
    assert_eq!(job.created_at, job.updated_at);

    let fetched = upload_jobs::get(&pool, &job.id).await.expect("get");
    assert_eq!(fetched.id, job.id);
}

#[tokio::test]
async fn get_unknown_is_404() {
    let (_dir, pool) = setup().await;
    let err = upload_jobs::get(&pool, "does-not-exist").await.unwrap_err();
    assert!(matches!(
        err,
        zukka_scheduler::error::ApiError::NotFound { .. }
    ));
}

#[tokio::test]
async fn patch_state_happy_path_and_attempts() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let track = seed_track(&pool, &caster).await;
    let job = upload_jobs::create(&pool, &new_job(&caster)).await.unwrap();

    let running = upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "running".into(),
            sha256: Some("abc123".into()),
            track_id: None,
            error: None,
        },
    )
    .await
    .expect("pending -> running");
    assert_eq!(running.state, "running");
    assert_eq!(running.sha256.as_deref(), Some("abc123"));
    assert_eq!(running.attempts, 1, "entering running increments attempts");

    let done = upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "done".into(),
            sha256: None,
            track_id: Some(track.clone()),
            error: None,
        },
    )
    .await
    .expect("running -> done");
    assert_eq!(done.state, "done");
    assert_eq!(done.track_id.as_deref(), Some(track.as_str()));
    // sha256 survives because the patch omitted it.
    assert_eq!(done.sha256.as_deref(), Some("abc123"));
    assert_eq!(done.attempts, 1);
}

#[tokio::test]
async fn illegal_transition_is_conflict() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let job = upload_jobs::create(&pool, &new_job(&caster)).await.unwrap();

    // pending -> done is not allowed (must pass through running).
    let err = upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "done".into(),
            sha256: None,
            track_id: None,
            error: None,
        },
    )
    .await
    .unwrap_err();
    match err {
        zukka_scheduler::error::ApiError::Conflict { code, .. } => {
            assert_eq!(code, "invalid_state_transition");
        }
        other => panic!("expected conflict, got {other:?}"),
    }

    let track = seed_track(&pool, &caster).await;
    // done -> running is terminal and must fail too.
    upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "running".into(),
            sha256: None,
            track_id: None,
            error: None,
        },
    )
    .await
    .unwrap();
    upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "done".into(),
            sha256: None,
            track_id: Some(track),
            error: None,
        },
    )
    .await
    .unwrap();
    let err = upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "running".into(),
            sha256: None,
            track_id: None,
            error: None,
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
async fn find_by_hash_only_matches_done_with_track() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let track = seed_track(&pool, &caster).await;

    let pending = upload_jobs::create(
        &pool,
        &CreateUploadJobRequest {
            sha256: Some("hash-1".into()),
            ..new_job(&caster)
        },
    )
    .await
    .unwrap();
    // pending job with a hash is not a dedupe hit.
    assert!(
        upload_jobs::find_by_hash(&pool, "hash-1")
            .await
            .unwrap()
            .is_none()
    );

    upload_jobs::patch_state(
        &pool,
        &pending.id,
        &PatchUploadJobRequest {
            state: "running".into(),
            sha256: None,
            track_id: None,
            error: None,
        },
    )
    .await
    .unwrap();
    upload_jobs::patch_state(
        &pool,
        &pending.id,
        &PatchUploadJobRequest {
            state: "done".into(),
            sha256: None,
            track_id: Some(track.clone()),
            error: None,
        },
    )
    .await
    .unwrap();

    let hit = upload_jobs::find_by_hash(&pool, "hash-1")
        .await
        .unwrap()
        .expect("dedupe hit");
    assert_eq!(hit.track_id.as_deref(), Some(track.as_str()));
    assert!(
        upload_jobs::find_by_hash(&pool, "other")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn list_stale_running_filters_by_age() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;

    let job = upload_jobs::create(&pool, &new_job(&caster)).await.unwrap();
    upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "running".into(),
            sha256: None,
            track_id: None,
            error: None,
        },
    )
    .await
    .unwrap();

    // Cutoff in the far future ⇒ everything running is "stale".
    let stale = upload_jobs::list_stale_running(&pool, "2999-01-01T00:00:00.000Z")
        .await
        .unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].id, job.id);

    // Cutoff in the far past ⇒ nothing is stale yet.
    let fresh = upload_jobs::list_stale_running(&pool, "2000-01-01T00:00:00.000Z")
        .await
        .unwrap();
    assert!(fresh.is_empty());
}

// ---------------------------------------------------------------------------
// US-10 — startup recovery
// ---------------------------------------------------------------------------

/// Moves a job to `running`, then backdates `updated_at` so it looks stale.
async fn make_stale_running(pool: &SqlitePool, caster: &str, attempts: i64) -> String {
    let job = upload_jobs::create(
        pool,
        &CreateUploadJobRequest {
            caster_id: caster.to_string(),
            filename: "stale.flac".into(),
            size_bytes: None,
            sha256: None,
        },
    )
    .await
    .unwrap();
    for _ in 0..attempts {
        upload_jobs::patch_state(
            pool,
            &job.id,
            &PatchUploadJobRequest {
                state: "running".into(),
                sha256: None,
                track_id: None,
                error: None,
            },
        )
        .await
        .unwrap();
    }
    sqlx::query("UPDATE upload_jobs SET updated_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(&job.id)
        .execute(pool)
        .await
        .unwrap();
    job.id
}

#[tokio::test]
async fn recovery_requeues_then_fails_by_attempts() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;

    let requeue = make_stale_running(&pool, &caster, 1).await;
    let give_up = make_stale_running(&pool, &caster, 3).await;

    let report = recovery::recover_stale_uploads(&pool, 1800, 3)
        .await
        .unwrap();
    assert_eq!(report.requeued, 1);
    assert_eq!(report.failed, 1);

    assert_eq!(
        upload_jobs::get(&pool, &requeue).await.unwrap().state,
        "pending"
    );
    let failed = upload_jobs::get(&pool, &give_up).await.unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.error.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn recovery_ignores_fresh_running_jobs() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;

    // running, but updated_at is now → not stale for a 30-min timeout.
    let job = upload_jobs::create(&pool, &new_job(&caster)).await.unwrap();
    upload_jobs::patch_state(
        &pool,
        &job.id,
        &PatchUploadJobRequest {
            state: "running".into(),
            sha256: None,
            track_id: None,
            error: None,
        },
    )
    .await
    .unwrap();

    let report = recovery::recover_stale_uploads(&pool, 1800, 3)
        .await
        .unwrap();
    assert_eq!(report, recovery::RecoveryReport::default());
    assert_eq!(
        upload_jobs::get(&pool, &job.id).await.unwrap().state,
        "running"
    );
}

// ---------------------------------------------------------------------------
// US-2 — HTTP endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_requires_service_token() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let (status, _) = send(
        &app(pool.clone()),
        Method::POST,
        "/api/v1/internal/upload-jobs",
        Some(json!({"caster_id": caster, "filename": "a.mp3"})),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_patch_roundtrip() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let app = app(pool.clone());

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/upload-jobs",
        Some(json!({"caster_id": caster, "filename": "a.mp3", "size_bytes": 42})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body["id"].as_str().expect("id").to_string();
    assert_eq!(id.len(), 36);

    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "pending");

    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        Some(json!({"state": "running", "sha256": "deadbeef"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "running");
    assert_eq!(body["attempts"], 1);
}

#[tokio::test]
async fn create_missing_fields_is_422() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let app = app(pool.clone());

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/upload-jobs",
        Some(json!({"caster_id": caster, "filename": "   "})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "filename");
}

#[tokio::test]
async fn patch_unknown_is_404_and_bad_transition_is_409() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let app = app(pool.clone());

    let (status, _) = send(
        &app,
        Method::PATCH,
        "/api/v1/internal/upload-jobs/0192aaaa-0000-7000-8000-000000000000",
        Some(json!({"state": "running"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, body) = send(
        &app,
        Method::POST,
        "/api/v1/internal/upload-jobs",
        Some(json!({"caster_id": caster, "filename": "a.mp3"})),
        true,
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();

    // pending -> done is illegal.
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        Some(json!({"state": "done"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "invalid_state_transition");

    // unknown state → 422.
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/internal/upload-jobs/{id}"),
        Some(json!({"state": "wat"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "state");
}

#[tokio::test]
async fn list_returns_newest_first() {
    let (_dir, pool) = setup().await;
    let caster = seed_caster(&pool).await;
    let app = app(pool.clone());

    for name in ["a.mp3", "b.mp3", "c.mp3"] {
        send(
            &app,
            Method::POST,
            "/api/v1/internal/upload-jobs",
            Some(json!({"caster_id": caster, "filename": name})),
            true,
        )
        .await;
    }
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/v1/internal/upload-jobs?limit=2",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body.as_array().expect("array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["filename"], "c.mp3");
    assert_eq!(items[1]["filename"], "b.mp3");
}
