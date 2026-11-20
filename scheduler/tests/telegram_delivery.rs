//! End-to-end announcement delivery tests (Issue #37).
//!
//! Covers the dispatcher contract against SQLite with a `Noop`/`Failing`
//! transport (US-4) and the real [`TelegramTransport`] against a local mock
//! HTTP server (US-7). No test performs a real network call.
//!
//! The mock speaks the Telegram Bot API shape: `POST /bot<token>/sendMessage`
//! with a JSON body. Tests assert queue state (`status`, `attempts`,
//! `next_attempt_at`, `error`), the `event_log` audit rows, and that the token
//! never leaks into error messages.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use zukka_scheduler::announcement::delivery::{DeliveryError, OutboundAnnouncement, Transport};
use zukka_scheduler::announcement::dispatch::run_once;
use zukka_scheduler::announcement::telegram::{TelegramConfig, TelegramTransport};

const TOKEN: &str = "123456:TEST-SECRET-TOKEN";
const CHAT: &str = "424242";
const TS: &str = "2026-11-20T12:00:00.000Z";

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

struct Noop {
    calls: Arc<AtomicUsize>,
}

impl Transport for Noop {
    fn send(
        &self,
        _msg: OutboundAnnouncement,
    ) -> Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct Failing;

impl Transport for Failing {
    fn send(
        &self,
        _msg: OutboundAnnouncement,
    ) -> Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send>> {
        Box::pin(async { Err(DeliveryError::new("transport boom")) })
    }
}

// ---------------------------------------------------------------------------
// Mock Telegram API
// ---------------------------------------------------------------------------

struct Mock {
    base: String,
    handle: JoinHandle<Vec<String>>,
}

/// Serves `count` requests with `status`/`body`, recording raw requests.
async fn mock_server(count: usize, status: u16, body: &'static str) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        for _ in 0..count {
            let (mut socket, _) = listener.accept().await.expect("accept");
            seen.push(read_request(&mut socket).await);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
        seen
    });
    Mock {
        base: format!("http://{addr}"),
        handle,
    }
}

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

fn request_body(request: &str) -> String {
    request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

async fn insert_template(pool: &SqlitePool, id: &str, body: &str) {
    sqlx::query(
        "INSERT INTO announcement_templates \
         (id, name, event, show_id, channel, body, enabled, created_at, updated_at) \
         VALUES (?, 'Test', 'manual', NULL, 'telegram', ?, 1, ?, ?)",
    )
    .bind(id)
    .bind(body)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert template");
}

async fn insert_announcement(
    pool: &SqlitePool,
    id: &str,
    template_id: Option<&str>,
    payload: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO announcements \
         (id, event, channel, payload, template_id, status, attempts, created_at, updated_at) \
         VALUES (?, 'manual', 'telegram', ?, ?, 'pending', 0, ?, ?)",
    )
    .bind(id)
    .bind(payload.to_string())
    .bind(template_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert announcement");
}

struct State {
    status: String,
    attempts: i64,
    next_attempt_at: Option<String>,
    error: Option<String>,
    sent_at: Option<String>,
}

async fn state(pool: &SqlitePool, id: &str) -> State {
    let row: (String, i64, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT status, attempts, next_attempt_at, error, sent_at FROM announcements WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("fetch state");
    State {
        status: row.0,
        attempts: row.1,
        next_attempt_at: row.2,
        error: row.3,
        sent_at: row.4,
    }
}

async fn event_count(pool: &SqlitePool, event: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM event_log WHERE event = ?")
        .bind(event)
        .fetch_one(pool)
        .await
        .expect("count events")
}

// ---------------------------------------------------------------------------
// US-4: dispatcher against SQLite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn noop_transport_marks_sent_and_logs() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "Hallo {caster_name}!").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({"caster_name": "Momo"})).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let report = run_once(
        &pool,
        &Noop {
            calls: calls.clone(),
        },
    )
    .await
    .expect("dispatch");
    assert_eq!(report.sent, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let state = state(&pool, "a-1").await;
    assert_eq!(state.status, "sent");
    assert!(state.sent_at.is_some());
    assert_eq!(event_count(&pool, "announcement_sent").await, 1);
}

#[tokio::test]
async fn failing_transport_marks_failed_with_backoff() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "x").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({})).await;

    let report = run_once(&pool, &Failing).await.expect("dispatch");
    assert_eq!(report.failed, 1);

    let state = state(&pool, "a-1").await;
    assert_eq!(state.status, "pending");
    assert_eq!(state.attempts, 1);
    assert_eq!(state.error.as_deref(), Some("transport boom"));
    let next = state.next_attempt_at.expect("next_attempt_at");
    assert!(next.as_str() > TS, "backoff must move into the future");
    assert_eq!(event_count(&pool, "announcement_failed").await, 1);
}

#[tokio::test]
async fn repeated_failures_become_terminal() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "x").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({})).await;

    // Re-arm manually between passes: the backoff would otherwise hide the row.
    for _ in 0..5 {
        sqlx::query("UPDATE announcements SET next_attempt_at = NULL WHERE id = 'a-1'")
            .execute(&pool)
            .await
            .expect("re-arm");
        run_once(&pool, &Failing).await.expect("dispatch");
    }

    let state = state(&pool, "a-1").await;
    assert_eq!(state.status, "failed");
    assert_eq!(state.attempts, 5);
    assert!(state.next_attempt_at.is_none());
    assert_eq!(event_count(&pool, "announcement_failed").await, 5);
}

#[tokio::test]
async fn a_failure_does_not_abort_the_batch() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "x").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({})).await;
    insert_announcement(&pool, "a-2", Some("tpl-1"), json!({})).await;

    let report = run_once(&pool, &Failing).await.expect("dispatch");
    assert_eq!(report.failed, 2);
    assert_eq!(state(&pool, "a-1").await.attempts, 1);
    assert_eq!(state(&pool, "a-2").await.attempts, 1);
}

#[tokio::test]
async fn no_due_rows_means_no_send() {
    let (_dir, pool) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let report = run_once(
        &pool,
        &Noop {
            calls: calls.clone(),
        },
    )
    .await
    .expect("dispatch");
    assert_eq!(
        report,
        zukka_scheduler::announcement::dispatch::DispatchReport::default()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_template_is_a_delivery_error_without_http() {
    let (_dir, pool) = setup().await;
    insert_announcement(&pool, "a-1", None, json!({})).await;

    let transport = TelegramTransport::new(
        pool.clone(),
        TelegramConfig {
            token: TOKEN.to_string(),
            chat_id: CHAT.to_string(),
            api_base: "http://127.0.0.1:1".to_string(),
            channel: zukka_scheduler::announcement::telegram::ChannelKind::Main,
        },
    );
    let report = run_once(&pool, &transport).await.expect("dispatch");
    assert_eq!(report.failed, 1);
    let state = state(&pool, "a-1").await;
    assert_eq!(state.attempts, 1);
    let error = state.error.expect("error");
    assert!(error.contains("template"), "unexpected error: {error}");
    assert!(!error.contains(TOKEN));
}

// ---------------------------------------------------------------------------
// US-7: Telegram transport against mock HTTP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_200_renders_text_and_marks_sent() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "Hallo {caster_name}!").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({"caster_name": "Momo"})).await;

    let mock = mock_server(1, 200, r#"{"ok":true,"result":{}}"#).await;
    let transport = TelegramTransport::new(
        pool.clone(),
        TelegramConfig {
            token: TOKEN.to_string(),
            chat_id: CHAT.to_string(),
            api_base: mock.base.clone(),
            channel: zukka_scheduler::announcement::telegram::ChannelKind::Main,
        },
    );
    let report = run_once(&pool, &transport).await.expect("dispatch");
    assert_eq!(report.sent, 1);

    let requests = mock.handle.await.expect("mock join");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with(&format!("POST /bot{TOKEN}/sendMessage")),
        "unexpected request line: {}",
        requests[0]
    );
    let body = request_body(&requests[0]);
    assert!(body.contains("\"chat_id\""), "body: {body}");
    assert!(body.contains(CHAT), "body: {body}");
    assert!(body.contains("Hallo Momo!"), "body: {body}");

    let state = state(&pool, "a-1").await;
    assert_eq!(state.status, "sent");
    assert!(state.sent_at.is_some());
    assert_eq!(event_count(&pool, "announcement_sent").await, 1);
}

#[tokio::test]
async fn http_400_marks_failed_and_hides_token() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "Hallo").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({})).await;

    let mock = mock_server(
        1,
        400,
        r#"{"ok":false,"description":"Bad Request: chat not found"}"#,
    )
    .await;
    let transport = TelegramTransport::new(
        pool.clone(),
        TelegramConfig {
            token: TOKEN.to_string(),
            chat_id: CHAT.to_string(),
            api_base: mock.base.clone(),
            channel: zukka_scheduler::announcement::telegram::ChannelKind::Main,
        },
    );
    let report = run_once(&pool, &transport).await.expect("dispatch");
    assert_eq!(report.failed, 1);
    mock.handle.await.expect("mock join");

    let state = state(&pool, "a-1").await;
    assert_eq!(state.status, "pending");
    assert_eq!(state.attempts, 1);
    assert!(state.next_attempt_at.is_some());
    let error = state.error.expect("error");
    assert!(error.contains("400"), "error: {error}");
    assert!(error.contains("chat not found"), "error: {error}");
    assert!(!error.contains(TOKEN), "token leaked: {error}");
    assert!(!error.contains(&mock.base), "url leaked: {error}");
    assert_eq!(event_count(&pool, "announcement_failed").await, 1);
}

#[tokio::test]
async fn http_200_with_ok_false_is_an_error() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "Hallo").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({})).await;

    let mock = mock_server(1, 200, r#"{"ok":false,"description":"Forbidden"}"#).await;
    let transport = TelegramTransport::new(
        pool.clone(),
        TelegramConfig {
            token: TOKEN.to_string(),
            chat_id: CHAT.to_string(),
            api_base: mock.base.clone(),
            channel: zukka_scheduler::announcement::telegram::ChannelKind::Main,
        },
    );
    let report = run_once(&pool, &transport).await.expect("dispatch");
    assert_eq!(report.failed, 1);
    mock.handle.await.expect("mock join");

    let error = state(&pool, "a-1").await.error.expect("error");
    assert!(error.contains("Forbidden"), "error: {error}");
    assert!(!error.contains(TOKEN));
}

#[tokio::test]
async fn network_error_does_not_leak_token() {
    let (_dir, pool) = setup().await;
    insert_template(&pool, "tpl-1", "Hallo").await;
    insert_announcement(&pool, "a-1", Some("tpl-1"), json!({})).await;

    // Grab a free port, then drop the listener so connect fails fast.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let transport = TelegramTransport::new(
        pool.clone(),
        TelegramConfig {
            token: TOKEN.to_string(),
            chat_id: CHAT.to_string(),
            api_base: format!("http://{addr}"),
            channel: zukka_scheduler::announcement::telegram::ChannelKind::Main,
        },
    );
    let report = run_once(&pool, &transport).await.expect("dispatch");
    assert_eq!(report.failed, 1);

    let error = state(&pool, "a-1").await.error.expect("error");
    assert!(!error.contains(TOKEN), "token leaked: {error}");
    assert!(!error.contains(&addr.to_string()), "url leaked: {error}");
    assert!(error.contains("telegram sendMessage"), "error: {error}");
}

#[tokio::test]
async fn spawn_is_disabled_without_configuration() {
    // Safe: this is the only test in this binary that touches the process
    // environment; all other tests pass explicit configs.
    unsafe {
        std::env::remove_var(zukka_scheduler::announcement::telegram::BOT_TOKEN_ENV);
        std::env::remove_var(zukka_scheduler::announcement::telegram::CHAT_ID_ENV);
    }
    assert!(zukka_scheduler::announcement::dispatch::spawn(setup().await.1).is_none());
}
