//! Hermetic ingest tests (#132): staged MP3 → mock music API → tapes/tape_jobs.
//!
//! The mock speaks the music API contract (`HEAD`/`PUT /objects/{sha256}`) on a
//! local `TcpListener`; every response is scripted and every request is
//! recorded. No test performs a real network call and no process env is needed
//! (the config is injected).

use std::path::PathBuf;
use std::time::Duration;

use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use zukka_scheduler::tape_ingest::{self, IngestConfig};

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const TAPE: &str = "0192aaaa-0001-7000-8000-00000000000a";
const JOB: &str = "0192bbbb-0001-7000-8000-00000000000b";
const CONTENT: &[u8] = b"hello zukka";
const CONTENT_SHA: &str = "3906f486a8fdf4ebebdece160621598c37057af3f5baad389dd00053f64be8b9";

// ---------------------------------------------------------------------------
// Scripted mock music API
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

/// Serves one scripted response per connection. `Connection: close` makes the
/// client open a fresh socket per request, so the queue maps 1:1 to requests.
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

fn request_line(request: &str) -> (String, String) {
    let first = request.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

fn header_value(request: &str, name: &str) -> Option<String> {
    let needle = format!("{}:", name.to_ascii_lowercase());
    request
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let lowered = line.to_ascii_lowercase();
            lowered
                .starts_with(&needle)
                .then(|| line[needle.len()..].trim().to_string())
        })
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    dir: TempDir,
    pool: SqlitePool,
    recordings: PathBuf,
}

async fn setup() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES ('c-tape', 'tapeuser', 'Tape User', 'x', ?, 1, ?, ?)",
    )
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert caster");
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, status, visible, \
         created_at, updated_at) \
         VALUES (?, 'slug1234', 'c-tape', '2026-11-23T10:00:00.000Z', \
         '2026-11-23T12:00:00.000Z', 'live', 'pending', 0, ?, ?)",
    )
    .bind(TAPE)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert tape");
    sqlx::query(
        "INSERT INTO tape_jobs (id, tape_id, kind, state, attempts, created_at, updated_at) \
         VALUES (?, ?, 'upload', 'pending', 0, ?, ?)",
    )
    .bind(JOB)
    .bind(TAPE)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert upload job");

    let recordings = dir.path().join("recordings");
    tokio::fs::create_dir_all(&recordings)
        .await
        .expect("recordings dir");
    tokio::fs::write(recordings.join(format!("{TAPE}.mp3")), CONTENT)
        .await
        .expect("staging file");

    Fixture {
        dir,
        pool,
        recordings,
    }
}

fn config(fixture: &Fixture, base: &str) -> IngestConfig {
    IngestConfig {
        music_api_base: base.to_string(),
        token: "test-token".into(),
        staging_root: fixture.recordings.clone(),
    }
}

async fn job_state(pool: &SqlitePool) -> (String, Option<String>, i64) {
    sqlx::query_as("SELECT state, error, attempts FROM tape_jobs WHERE id = ?")
        .bind(JOB)
        .fetch_one(pool)
        .await
        .expect("job row")
}

async fn tape_fields(
    pool: &SqlitePool,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    sqlx::query_as(
        "SELECT status, music_api_id, storage_key, size_bytes, etag, format, mime \
         FROM tapes WHERE id = ?",
    )
    .bind(TAPE)
    .fetch_one(pool)
    .await
    .expect("tape row")
}

// ---------------------------------------------------------------------------
// T1 — happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_uploads_and_completes() {
    let fixture = setup().await;
    let mock = mock_api(vec![
        reply(404),
        Reply {
            status: 200,
            etag: Some("\"abc123\""),
        },
    ])
    .await;

    let report = tape_ingest::run_once(&fixture.pool, &config(&fixture, &mock.base))
        .await
        .expect("run_once");
    assert_eq!(report.uploaded, 1);

    let seen = mock.handle.await.expect("mock join");

    // HEAD first, then PUT with the hash in the path.
    let (method, path) = request_line(&seen[0]);
    assert_eq!(method, "HEAD");
    assert_eq!(path, format!("/objects/{CONTENT_SHA}"));
    assert_eq!(
        header_value(&seen[0], "authorization").as_deref(),
        Some("Bearer test-token")
    );

    let (method, path) = request_line(&seen[1]);
    assert_eq!(method, "PUT");
    assert_eq!(path, format!("/objects/{CONTENT_SHA}"));
    assert_eq!(
        header_value(&seen[1], "x-name").as_deref(),
        Some("slug1234")
    );
    let expected_origin = format!("/recordings/{TAPE}.mp3");
    assert_eq!(
        header_value(&seen[1], "x-original-path").as_deref(),
        Some(expected_origin.as_str())
    );
    assert!(
        seen[1].contains("hello zukka"),
        "PUT body must stream the staging file"
    );
    assert_eq!(
        header_value(&seen[1], "content-length").as_deref(),
        Some(CONTENT.len().to_string().as_str()),
        "PUT must announce the streamed size"
    );
    assert!(
        !seen[1].to_ascii_lowercase().contains("transfer-encoding:"),
        "Content-Length replaces chunked encoding"
    );

    let (state, error, attempts) = job_state(&fixture.pool).await;
    assert_eq!(state, "done");
    assert_eq!(error, None);
    assert_eq!(attempts, 1);

    let (status, music_api_id, storage_key, size_bytes, etag, format, mime) =
        tape_fields(&fixture.pool).await;
    assert_eq!(status, "processing");
    assert_eq!(music_api_id.as_deref(), Some(CONTENT_SHA));
    assert_eq!(storage_key.as_deref(), Some(CONTENT_SHA));
    assert_eq!(size_bytes, Some(CONTENT.len() as i64));
    assert_eq!(etag.as_deref(), Some("\"abc123\""));
    assert_eq!(format.as_deref(), Some("mp3"));
    assert_eq!(mime.as_deref(), Some("audio/mpeg"));

    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tape_jobs WHERE tape_id = ? AND kind IN ('probe','peaks') \
         AND state = 'pending'",
    )
    .bind(TAPE)
    .fetch_one(&fixture.pool)
    .await
    .expect("count");
    assert_eq!(pending, 2);

    assert!(
        !fixture.recordings.join(format!("{TAPE}.mp3")).exists(),
        "staging file must be deleted only after completion"
    );
    drop(fixture.dir);
}

// ---------------------------------------------------------------------------
// T2 — dedupe via HEAD 200
// ---------------------------------------------------------------------------

#[tokio::test]
async fn existing_object_skips_put() {
    let fixture = setup().await;
    let mock = mock_api(vec![reply(200)]).await;

    let report = tape_ingest::run_once(&fixture.pool, &config(&fixture, &mock.base))
        .await
        .expect("run_once");
    assert_eq!(report.deduped, 1);

    let seen = mock.handle.await.expect("mock join");
    assert_eq!(seen.len(), 1, "no PUT after a 200 HEAD");

    let (status, music_api_id, _, size_bytes, etag, _, _) = tape_fields(&fixture.pool).await;
    assert_eq!(status, "processing");
    assert_eq!(music_api_id.as_deref(), Some(CONTENT_SHA));
    assert_eq!(size_bytes, Some(CONTENT.len() as i64));
    assert_eq!(
        etag.as_deref(),
        Some(CONTENT_SHA),
        "etag falls back to the hash"
    );

    let (state, _, _) = job_state(&fixture.pool).await;
    assert_eq!(state, "done");
    assert!(!fixture.recordings.join(format!("{TAPE}.mp3")).exists());
    drop(fixture.dir);
}

// ---------------------------------------------------------------------------
// T3 — PUT 5xx keeps job + file retryable, then requeue succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_failure_keeps_job_and_file_then_requeue_succeeds() {
    let fixture = setup().await;
    let failing = mock_api(vec![reply(404), reply(500)]).await;

    let report = tape_ingest::run_once(&fixture.pool, &config(&fixture, &failing.base))
        .await
        .expect("run_once");
    assert_eq!(report.failed, 1);

    let (state, error, attempts) = job_state(&fixture.pool).await;
    assert_eq!(
        state, "pending",
        "a retryable failure requeues the job automatically"
    );
    assert_eq!(attempts, 1);
    assert!(
        error.as_deref().unwrap_or_default().contains("500"),
        "error must name the status: {error:?}"
    );

    let (status, music_api_id, _, _, _, _, _) = tape_fields(&fixture.pool).await;
    assert_eq!(status, "pending", "tape status must stay untouched");
    assert_eq!(music_api_id, None);
    assert!(
        fixture.recordings.join(format!("{TAPE}.mp3")).exists(),
        "staging file must survive a failure"
    );

    // Requeue: `fail_upload` already set the job back to `pending`; only the
    // backoff window is aged here (never the state), so the real claim retries.
    sqlx::query(
        "UPDATE tape_jobs SET attempts = 0, updated_at = '1970-01-01T00:00:00.000Z' WHERE id = ?",
    )
    .bind(JOB)
    .execute(&fixture.pool)
    .await
    .expect("age backoff");

    let healthy = mock_api(vec![reply(404), reply(200)]).await;
    let report = tape_ingest::run_once(&fixture.pool, &config(&fixture, &healthy.base))
        .await
        .expect("run_once retry");
    assert_eq!(report.uploaded, 1);

    let (state, error, _) = job_state(&fixture.pool).await;
    assert_eq!(state, "done");
    assert_eq!(error, None);
    let (status, music_api_id, _, _, _, _, _) = tape_fields(&fixture.pool).await;
    assert_eq!(status, "processing");
    assert_eq!(music_api_id.as_deref(), Some(CONTENT_SHA));
    assert!(!fixture.recordings.join(format!("{TAPE}.mp3")).exists());
    drop(fixture.dir);
}

// ---------------------------------------------------------------------------
// T4 — unreachable server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unreachable_server_keeps_job_and_file() {
    let fixture = setup().await;
    // Bind then drop: the port is almost certainly closed.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let base = format!("http://127.0.0.1:{port}");
    let cfg = IngestConfig {
        music_api_base: base,
        token: "test-token".into(),
        staging_root: fixture.recordings.clone(),
    };

    let report = tokio::time::timeout(
        Duration::from_secs(10),
        tape_ingest::run_once(&fixture.pool, &cfg),
    )
    .await
    .expect("run_once must not hang")
    .expect("run_once");
    assert_eq!(report.failed, 1);

    let (state, error, _) = job_state(&fixture.pool).await;
    assert_eq!(
        state, "pending",
        "a retryable failure requeues the job automatically"
    );
    assert!(error.is_some());
    let (status, _, _, _, _, _, _) = tape_fields(&fixture.pool).await;
    assert_eq!(status, "pending");
    assert!(fixture.recordings.join(format!("{TAPE}.mp3")).exists());
    drop(fixture.dir);
}

// ---------------------------------------------------------------------------
// T5 — attempt cap marks the job and tape terminal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attempt_cap_fails_job_and_tape() {
    let fixture = setup().await;
    // The next claim raises attempts to the cap, so this failure is terminal.
    sqlx::query(
        "UPDATE tape_jobs SET attempts = 7, updated_at = '1970-01-01T00:00:00.000Z' WHERE id = ?",
    )
    .bind(JOB)
    .execute(&fixture.pool)
    .await
    .expect("prime attempts");

    let mock = mock_api(vec![reply(404), reply(500)]).await;
    let report = tape_ingest::run_once(&fixture.pool, &config(&fixture, &mock.base))
        .await
        .expect("run_once");
    assert_eq!(report.failed, 1);

    let (state, error, attempts) = job_state(&fixture.pool).await;
    assert_eq!(state, "failed", "attempt cap is terminal");
    assert_eq!(attempts, 8);
    assert!(error.as_deref().unwrap_or_default().contains("500"));
    let (status, _, _, _, _, _, _) = tape_fields(&fixture.pool).await;
    assert_eq!(status, "failed");
    assert!(
        fixture.recordings.join(format!("{TAPE}.mp3")).exists(),
        "staging file stays for manual recovery"
    );
    drop(fixture.dir);
}
