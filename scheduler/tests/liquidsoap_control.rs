//! Push-channel integration test (issue #32).
//!
//! One test binary, one process-wide env var: `ZUKKA_LIQUIDSOAP_CONTROL_URL`
//! points at a local mock HTTP server. Other test binaries (e.g.
//! `control_api.rs`) never set it, so their runs stay hermetic.
//!
//! The mock speaks the same shape as Liquidsoap's command server: `POST
//! /control` with the command as body. It asserts that `POST /control/live`
//! really reaches Liquidsoap as `live.set <mount>` / `live.clear` — i.e. the
//! operator override acts immediately (§2.5).

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const CASTER: &str = "c0000000-0000-7000-8000-000000000001";
const MOUNT: &str = "/live/slot-07";
const TS: &str = "2026-11-19T12:00:00.000Z";

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

async fn setup(harbor_mount: Option<&str>) -> (TempDir, SqlitePool, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    sqlx::query(
        "INSERT INTO casters \
         (id, username, display_name, password_hash, role_id, harbor_mount, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?, ?)",
    )
    .bind(CASTER)
    .bind("user_control_push")
    .bind(ROLE_CASTER)
    .bind(harbor_mount)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert caster");
    let app = zukka_scheduler::api::router(pool.clone());
    (dir, pool, app)
}

#[tokio::test]
async fn live_set_and_clear_reach_liquidsoap_immediately() {
    let (url, server) = mock_control_server(2).await;
    // Safe: this test binary contains exactly one test, so no other thread
    // reads the process environment concurrently.
    unsafe { std::env::set_var(zukka_scheduler::liquidsoap::CONTROL_URL_ENV, &url) };

    let (_dir, _pool, app) = setup(Some(MOUNT)).await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/control/live")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "caster_id": CASTER, "reason": "Override" })).unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.expect("set response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/control/live")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "caster_id": null, "reason": "FCFS" })).unwrap(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.expect("clear response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

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
        requests[0].ends_with("live.set /live/slot-07"),
        "set must carry the caster mount: {}",
        requests[0]
    );
    assert!(
        requests[1].ends_with("live.clear"),
        "clear command: {}",
        requests[1]
    );
}
