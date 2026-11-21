//! Tape-recording integration test (issue #108).
//!
//! One test binary, one process-wide env var: `ZUKKA_LIQUIDSOAP_CONTROL_URL`
//! points at a local mock HTTP server. Covers the whole happy path in a single
//! test so the env var is set by exactly one thread:
//!
//! 1. `POST /broadcasts/{id}/start` on a `live` broadcast creates the `tapes`
//!    row (source='live', status='pending', broadcast_id set) and pushes
//!    `tape.start <uuidv7>`.
//! 2. `POST /broadcasts/{id}/end` finalizes it and pushes `tape.stop`.
//! 3. A second `end` is still `409 illegal_transition` (behaviour unchanged).
//! 4. A `playlist` start creates no tape.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER: &str = "0192aaaa-0001-7000-8000-00000000000a";
const SHOW: &str = "0192a0a0-0001-7000-8000-0000000000a0";
const QUEUE: &str = "0192d0d0-0001-7000-8000-0000000000d0";
const START: &str = "2026-11-18T20:00:00+01:00";

/// Minimal HTTP/1.1 server: reads `count` requests, answers `200`, and returns
/// the raw request lines so the test can assert method, path and body.
async fn mock_control_server(count: usize) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        for _ in 0..count {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            seen.push(request);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("write response");
        }
        seen
    });
    (format!("http://{addr}/control"), handle)
}

/// Reads one request until the declared body is complete.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = socket.read(&mut chunk).await.expect("read");
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buffer);
        if let Some(split) = text.find("\r\n\r\n") {
            let content_length = text[..split]
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .or_else(|| line.strip_prefix("Content-Length:"))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buffer.len() >= split + 4 + content_length {
                return text.into_owned();
            }
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

async fn setup() -> (TempDir, SqlitePool, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    sqlx::query(
        "INSERT INTO casters \
         (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, 'user_tape', 'Test Caster', 'hash', ?, ?, ?)",
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
         VALUES (?, 'tape-show', 'Show', '', '[]', ?, ?, ?, ?)",
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

async fn create_broadcast(app: &Router, payload: Value) -> String {
    let (status, body) = send(app, Method::POST, "/api/v1/broadcasts", Some(payload)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().expect("broadcast id").to_string()
}

#[tokio::test]
async fn live_start_end_register_and_finalize_tape() {
    let (url, server) = mock_control_server(2).await;
    // Safe: this test binary contains exactly one test.
    unsafe { std::env::set_var(zukka_scheduler::liquidsoap::CONTROL_URL_ENV, &url) };

    let (_dir, pool, app) = setup().await;

    // 1. A live broadcast: start creates the tape + pushes tape.start.
    let live_id = create_broadcast(
        &app,
        json!({
            "caster_id": CASTER, "show_id": SHOW, "kind": "live",
            "title": "Live Slot", "starts_at": START
        }),
    )
    .await;

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{live_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "live");

    let row: (String, String, String, String, String) = sqlx::query_as(
        "SELECT id, source, status, broadcast_id, slug FROM tapes WHERE broadcast_id = ?",
    )
    .bind(&live_id)
    .fetch_one(&pool)
    .await
    .expect("tape row exists");
    let (tape_id, source, tape_status, broadcast_id, slug) = row;
    assert_eq!(source, "live");
    assert_eq!(tape_status, "pending");
    assert_eq!(broadcast_id, live_id);
    assert_eq!(slug.len(), 8, "slug: {slug}");
    assert!(uuid::Uuid::parse_str(&tape_id).is_ok(), "uuid: {tape_id}");

    // Placeholder ended_at == started_at at start.
    let (started_at, ended_at): (String, String) =
        sqlx::query_as("SELECT started_at, ended_at FROM tapes WHERE id = ?")
            .bind(&tape_id)
            .fetch_one(&pool)
            .await
            .expect("tape timestamps");
    assert_eq!(started_at, ended_at, "placeholder end equals start");

    // 2. End finalizes it (ended_at moves forward) and pushes tape.stop.
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{live_id}/end"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ended");

    let (tape_status, new_end): (String, String) =
        sqlx::query_as("SELECT status, ended_at FROM tapes WHERE id = ?")
            .bind(&tape_id)
            .fetch_one(&pool)
            .await
            .expect("tape after end");
    assert_eq!(
        tape_status, "pending",
        "status stays pending (ffprobe later)"
    );
    assert!(new_end >= started_at, "ended_at must not move backwards");

    // 3. Second end is still an illegal transition.
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{live_id}/end"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "illegal_transition");

    // 4. A playlist start creates no tape.
    let playlist_id = create_broadcast(
        &app,
        json!({
            "caster_id": CASTER, "kind": "playlist", "title": "Pl",
            "queue_id": QUEUE, "starts_at": "2026-11-19T20:00:00+01:00",
            "ends_at": "2026-11-19T22:00:00+01:00"
        }),
    )
    .await;
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{playlist_id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tapes WHERE broadcast_id = ?")
        .bind(&playlist_id)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 0, "playlist must not create a tape");

    // The mock saw exactly the two recorder pushes, in order.
    let requests = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("mock server timed out")
        .expect("mock server task");
    assert_eq!(requests.len(), 2, "two control pushes");
    assert!(
        requests[0].starts_with("POST /control"),
        "unexpected request: {}",
        requests[0]
    );
    assert!(
        requests[0].ends_with(&format!("tape.start {tape_id}")),
        "start push must carry the tape id: {}",
        requests[0]
    );
    assert!(
        requests[1].ends_with("tape.stop"),
        "stop command: {}",
        requests[1]
    );
}
