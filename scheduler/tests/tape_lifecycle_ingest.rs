//! Lifecycle → ingest integration test (#132).
//!
//! Proves the gap fixed in this issue is closed: finalizing a live tape must
//! enqueue the `kind='upload'` job, otherwise the ingest task has nothing to
//! claim and live recordings are never uploaded.
//!
//! Flow (single test, no process env needed):
//!
//! 1. `POST /broadcasts/{id}/start` on a `live` broadcast registers the tape.
//! 2. `POST /broadcasts/{id}/end` finalizes it **and** leaves exactly one
//!    `tape_jobs (kind='upload', state='pending', attempts=0)` row.
//! 3. `tape_ingest::run_once` against a scripted mock uploads the staging file:
//!    the upload job is `done`, `tapes.status='processing'`, `probe`/`peaks`
//!    are `pending`.
//! 4. A repeated `finalize_live_tape` does not double-enqueue (ON CONFLICT).
//!
//! The liquidsoap control URL env is intentionally unset: `push_best_effort`
//! then logs "push disabled" instead of failing.

use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tower::ServiceExt;

use zukka_scheduler::store::tapes;
use zukka_scheduler::tape_ingest::{self, IngestConfig};

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER: &str = "0192aaaa-0001-7000-8000-00000000000a";
const SHOW: &str = "0192a0a0-0001-7000-8000-0000000000a0";
const QUEUE: &str = "0192d0d0-0001-7000-8000-0000000000d0";
const START: &str = "2026-11-18T20:00:00+01:00";
const CONTENT: &[u8] = b"hello zukka";
const CONTENT_SHA: &str = "3906f486a8fdf4ebebdece160621598c37057af3f5baad389dd00053f64be8b9";

// ---------------------------------------------------------------------------
// Scripted mock music API (HEAD/PUT /objects/{sha256})
// ---------------------------------------------------------------------------

struct Reply {
    status: u16,
    etag: Option<&'static str>,
}

fn reply(status: u16) -> Reply {
    Reply { status, etag: None }
}

struct MockApi {
    base: String,
    handle: JoinHandle<Vec<String>>,
}

async fn mock_api(replies: Vec<Reply>) -> MockApi {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.expect("accept");
            seen.push(read_request(&mut socket).await);
            let mut head = format!(
                "HTTP/1.1 {} X\r\nConnection: close\r\nContent-Length: 0\r\n",
                reply.status
            );
            if let Some(etag) = reply.etag {
                head.push_str(&format!("ETag: {etag}\r\n"));
            }
            head.push_str("\r\n");
            socket
                .write_all(head.as_bytes())
                .await
                .expect("write response");
            socket.flush().await.expect("flush");
        }
        seen
    });
    MockApi {
        base: format!("http://{addr}"),
        handle,
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).await.expect("read");
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if request_complete(&buffer) {
            break;
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn request_complete(buffer: &[u8]) -> bool {
    let Some(end) = find_headers_end(buffer) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
    if head.contains("transfer-encoding: chunked") {
        return chunked_complete(&buffer[end..]);
    }
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    buffer.len() >= end + content_length
}

fn chunked_complete(body: &[u8]) -> bool {
    let mut pos = 0;
    loop {
        let Some(line_end) = body[pos..].windows(2).position(|w| w == b"\r\n") else {
            return false;
        };
        let Ok(size_text) = std::str::from_utf8(&body[pos..pos + line_end]) else {
            return false;
        };
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            return false;
        };
        pos += line_end + 2;
        if size == 0 {
            return body.len() >= pos + 2;
        }
        if body.len() < pos + size + 2 {
            return false;
        }
        pos += size + 2;
    }
}

// ---------------------------------------------------------------------------
// API fixture
// ---------------------------------------------------------------------------

async fn setup() -> (TempDir, SqlitePool, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    sqlx::query(
        "INSERT INTO casters \
         (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, 'user_lifecycle', 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(CASTER)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert caster");
    sqlx::query(
        "INSERT INTO shows (id, slug, name, description, links, caster_id, created_by, created_at, updated_at) \
         VALUES (?, 'lifecycle-show', 'Show', '', '[]', ?, ?, ?, ?)",
    )
    .bind(SHOW)
    .bind(CASTER)
    .bind(CASTER)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert show");
    sqlx::query(
        "INSERT INTO queues (id, caster_id, name, description, kind, version, created_at, updated_at) \
         VALUES (?, ?, 'main', '', 'slot', 1, ?, ?)",
    )
    .bind(QUEUE)
    .bind(CASTER)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert queue");
    let app = zukka_scheduler::api::router(pool.clone());
    (dir, pool, app)
}

async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
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
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn upload_job(pool: &SqlitePool, tape_id: &str) -> (String, String, i64) {
    sqlx::query_as(
        "SELECT kind, state, attempts FROM tape_jobs WHERE tape_id = ? AND kind = 'upload'",
    )
    .bind(tape_id)
    .fetch_one(pool)
    .await
    .expect("upload job row")
}

async fn upload_job_count(pool: &SqlitePool, tape_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM tape_jobs WHERE tape_id = ? AND kind = 'upload'")
        .bind(tape_id)
        .fetch_one(pool)
        .await
        .expect("upload job count")
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn finalize_enqueues_upload_job_and_ingest_completes() {
    let (dir, pool, app) = setup().await;
    let recordings: PathBuf = dir.path().join("recordings");
    tokio::fs::create_dir_all(&recordings)
        .await
        .expect("recordings dir");

    // 1. Live broadcast: start registers the tape, end finalizes it.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/broadcasts",
        Some(json!({
            "caster_id": CASTER, "show_id": SHOW, "kind": "live",
            "title": "Lifecycle Slot", "starts_at": START
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let live_id = body["id"].as_str().expect("broadcast id").to_string();

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{live_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{live_id}/end"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let tape_id: String = sqlx::query_scalar("SELECT id FROM tapes WHERE broadcast_id = ?")
        .bind(&live_id)
        .fetch_one(&pool)
        .await
        .expect("tape row");

    // 2. The finalize enqueued exactly one pending upload job for this tape.
    let (kind, state, attempts) = upload_job(&pool, &tape_id).await;
    assert_eq!(kind, "upload");
    assert_eq!(state, "pending", "finalize must enqueue the ingest job");
    assert_eq!(attempts, 0);
    assert_eq!(upload_job_count(&pool, &tape_id).await, 1);

    // 3. A repeated finalize must not double-enqueue the upload job (the tape
    //    is still `pending` here — ingest has not run yet).
    let mut tx = pool.begin().await.expect("begin");
    let again = tapes::finalize_live_tape(&mut tx, &live_id, Utc::now())
        .await
        .expect("finalize again");
    tx.commit().await.expect("commit");
    assert_eq!(again.as_deref(), Some(tape_id.as_str()));
    assert_eq!(
        upload_job_count(&pool, &tape_id).await,
        1,
        "ON CONFLICT must keep a single upload job"
    );

    // 4. Ingest against the mock uploads the staged recording.
    tokio::fs::write(recordings.join(format!("{tape_id}.mp3")), CONTENT)
        .await
        .expect("staging file");
    let mock = mock_api(vec![
        reply(404),
        Reply {
            status: 200,
            etag: Some("\"abc123\""),
        },
    ])
    .await;
    let cfg = IngestConfig {
        music_api_base: mock.base.clone(),
        token: "test-token".into(),
        staging_root: recordings.clone(),
    };
    let report = tape_ingest::run_once(&pool, &cfg).await.expect("run_once");
    assert_eq!(report.uploaded, 1);
    let _seen = tokio::time::timeout(Duration::from_secs(5), mock.handle)
        .await
        .expect("mock timed out")
        .expect("mock join");

    let (_, state, attempts) = upload_job(&pool, &tape_id).await;
    assert_eq!(state, "done", "upload job completes");
    assert_eq!(attempts, 1);

    let (tape_status, music_api_id): (String, Option<String>) =
        sqlx::query_as("SELECT status, music_api_id FROM tapes WHERE id = ?")
            .bind(&tape_id)
            .fetch_one(&pool)
            .await
            .expect("tape after ingest");
    assert_eq!(tape_status, "processing");
    assert_eq!(music_api_id.as_deref(), Some(CONTENT_SHA));

    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tape_jobs WHERE tape_id = ? AND kind IN ('probe','peaks') \
         AND state = 'pending'",
    )
    .bind(&tape_id)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(pending, 2, "probe + peaks enqueued");
    assert!(
        !recordings.join(format!("{tape_id}.mp3")).exists(),
        "staging file removed after completion"
    );

    drop(dir);
}
