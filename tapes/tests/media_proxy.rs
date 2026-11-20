//! HTTP-level integration tests for the audio proxy `GET|HEAD /media/{file}`
//! (Issue #28, `docs/tapes.md` §3.7).
//!
//! The app runs through `tower::ServiceExt::oneshot` against a temporary SQLite
//! file; the Music API is replaced by a tiny in-process upstream on an
//! ephemeral port that honors `Range` and counts requests.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const BASE_URL: &str = "https://tapes.test";
const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-01T00:00:00.000Z";
const SIZE: usize = 8192;
const ETAG: &str = "\"abc\"";

// --- mock Music API upstream ------------------------------------------------

#[derive(Clone)]
struct MockState {
    bytes: Arc<Vec<u8>>,
    count: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

struct Mock {
    url: String,
    count: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

impl Mock {
    fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

/// Starts a mock object store serving `total` bytes at `/objects/{*key}`.
async fn start_mock(total: usize) -> Mock {
    let state = MockState {
        bytes: Arc::new(vec![0x41u8; total]),
        count: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let count = state.count.clone();
    let seen = state.seen.clone();
    let app = Router::new()
        .route("/objects/{*key}", get(mock_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Mock {
        url: format!("http://{addr}"),
        count,
        seen,
    }
}

async fn mock_handler(State(state): State<MockState>, headers: HeaderMap) -> Response {
    state.count.fetch_add(1, Ordering::SeqCst);
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    state.seen.lock().unwrap().push(range.clone());

    let total = state.bytes.len();
    if let Some(spec) = range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        if let Some((a, b)) = spec.split_once('-') {
            let start: usize = a.trim().parse().unwrap_or(0);
            let end: usize = b.trim().parse().unwrap_or(total.saturating_sub(1));
            if start >= total || start > end {
                let mut res = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
                    res.headers_mut().insert(header::CONTENT_RANGE, value);
                }
                return res;
            }
            let end = end.min(total - 1);
            let body = state.bytes[start..=end].to_vec();
            let mut res = Response::new(Body::from(body));
            *res.status_mut() = StatusCode::PARTIAL_CONTENT;
            let h = res.headers_mut();
            h.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).unwrap(),
            );
            h.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&(end - start + 1).to_string()).unwrap(),
            );
            h.insert(header::ETAG, HeaderValue::from_static(ETAG));
            return res;
        }
    }

    let mut res = Response::new(Body::from(state.bytes.as_ref().clone()));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&total.to_string()).unwrap(),
    );
    h.insert(header::ETAG, HeaderValue::from_static(ETAG));
    res
}

/// A URL that no server answers (connection refused).
fn dead_upstream() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}")
}

// --- app setup --------------------------------------------------------------

async fn seed(pool: &SqlitePool, tape_size: i64) {
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
         created_at, updated_at) VALUES ('c-alpha', 'alpha', 'Alpha', 'x', ?, 1, ?, ?)",
    )
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");

    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, title, started_at, ended_at, duration_ms, source, \
         format, mime, size_bytes, etag, storage_key, status, visible, published_at, created_at, \
         updated_at) \
         VALUES ('aaa111', 'aaa111', 'c-alpha', 'My Title', '2026-11-23T10:00:00.000Z', \
         '2026-11-23T12:00:00.000Z', 7194000, 'live', 'mp3', 'audio/mpeg', ?, ?, \
         'recordings/aaa111.mp3', 'ready', 1, '2026-11-23T12:04:11.000Z', ?, ?)",
    )
    .bind(tape_size)
    .bind(ETAG)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");

    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, title, started_at, ended_at, source, format, mime, \
         size_bytes, etag, storage_key, status, visible, created_at, updated_at) \
         VALUES ('hidden1', 'hidden1', 'c-alpha', 'Hidden', '2026-11-19T10:00:00.000Z', \
         '2026-11-19T11:00:00.000Z', 'live', 'mp3', 'audio/mpeg', ?, ?, \
         'recordings/hidden1.mp3', 'ready', 0, ?, ?)",
    )
    .bind(tape_size)
    .bind(ETAG)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert hidden tape");
}

/// Builds the app over a fresh temp DB pointing at `upstream_url`.
async fn setup(upstream_url: &str, tape_size: i64) -> (TempDir, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let writer = zukka_tapes::fixtures::create_db(&path)
        .await
        .expect("create db");
    seed(&writer, tape_size).await;
    writer.close().await;

    let pool = zukka_tapes::db::connect_read_only(&path)
        .await
        .expect("read-only pool");
    let app = zukka_tapes::api::router(
        pool,
        BASE_URL.to_string(),
        upstream_url.to_string(),
        "test-token".to_string(),
    );
    (dir, app)
}

// --- request helpers --------------------------------------------------------

fn build(method: Method, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

fn hv(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

// --- tests ------------------------------------------------------------------

#[tokio::test]
async fn full_get_serves_bytes_with_all_headers() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, body) = send(&app, build(Method::GET, "/media/aaa111.mp3", &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hv(&headers, "accept-ranges"), "bytes");
    assert_eq!(hv(&headers, "content-type"), "audio/mpeg");
    assert_eq!(hv(&headers, "content-length"), SIZE.to_string());
    assert_eq!(hv(&headers, "etag"), ETAG);
    assert_eq!(
        hv(&headers, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(hv(&headers, "access-control-allow-origin"), "*");
    assert_eq!(
        hv(&headers, "access-control-expose-headers"),
        "Content-Length, Content-Range, Accept-Ranges, ETag"
    );
    assert!(headers.get("content-encoding").is_none());
    assert_eq!(body.len(), SIZE);
    assert_eq!(mock.requests(), 1);
}

#[tokio::test]
async fn bounded_range_returns_206() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, body) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=0-1023")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        hv(&headers, "content-range"),
        format!("bytes 0-1023/{SIZE}")
    );
    assert_eq!(hv(&headers, "content-length"), "1024");
    assert_eq!(body.len(), 1024);
    assert_eq!(hv(&headers, "accept-ranges"), "bytes");
    // The normalized range is what reached the upstream.
    assert_eq!(
        mock.seen.lock().unwrap().last().unwrap().as_deref(),
        Some("bytes=0-1023")
    );
}

#[tokio::test]
async fn suffix_range_returns_last_bytes() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, body) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=-4096")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        hv(&headers, "content-range"),
        format!("bytes {}-{}/{SIZE}", SIZE - 4096, SIZE - 1)
    );
    assert_eq!(hv(&headers, "content-length"), "4096");
    assert_eq!(body.len(), 4096);
}

#[tokio::test]
async fn open_ended_range_runs_to_eof() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, _body) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=8000-")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        hv(&headers, "content-range"),
        format!("bytes 8000-{}/{SIZE}", SIZE - 1)
    );
    assert_eq!(hv(&headers, "content-length"), "192");
}

#[tokio::test]
async fn range_past_eof_is_416_without_upstream_call() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, _body) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=8192-")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(hv(&headers, "content-range"), format!("bytes */{SIZE}"));
    assert_eq!(mock.requests(), 0);
}

#[tokio::test]
async fn multi_range_and_unknown_unit_serve_full() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    for range in ["bytes=0-99,200-299", "items=0-1"] {
        let (status, headers, body) = send(
            &app,
            build(Method::GET, "/media/aaa111.mp3", &[("range", range)]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{range}");
        assert_eq!(hv(&headers, "content-length"), SIZE.to_string(), "{range}");
        assert_eq!(body.len(), SIZE, "{range}");
    }
}

#[tokio::test]
async fn if_range_matches_or_falls_back() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, _b) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=0-1023"), ("if-range", ETAG)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        hv(&headers, "content-range"),
        format!("bytes 0-1023/{SIZE}")
    );

    let (status, headers, body) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=0-1023"), ("if-range", "\"stale\"")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hv(&headers, "content-length"), SIZE.to_string());
    assert_eq!(body.len(), SIZE);
}

#[tokio::test]
async fn if_none_match_returns_304_without_upstream_call() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, body) = send(
        &app,
        build(Method::GET, "/media/aaa111.mp3", &[("if-none-match", ETAG)]),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    assert_eq!(hv(&headers, "etag"), ETAG);
    assert_eq!(
        hv(&headers, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(mock.requests(), 0);
}

#[tokio::test]
async fn head_never_touches_upstream() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, body) = send(&app, build(Method::HEAD, "/media/aaa111.mp3", &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(hv(&headers, "content-length"), SIZE.to_string());
    assert_eq!(hv(&headers, "content-type"), "audio/mpeg");
    assert_eq!(hv(&headers, "accept-ranges"), "bytes");
    assert_eq!(hv(&headers, "etag"), ETAG);
    assert_eq!(mock.requests(), 0);

    // A ranged HEAD reflects the range without downloading anything.
    let (status, headers, body) = send(
        &app,
        build(
            Method::HEAD,
            "/media/aaa111.mp3",
            &[("range", "bytes=0-1023")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert!(body.is_empty());
    assert_eq!(
        hv(&headers, "content-range"),
        format!("bytes 0-1023/{SIZE}")
    );
    assert_eq!(hv(&headers, "content-length"), "1024");
    assert_eq!(mock.requests(), 0);
}

#[tokio::test]
async fn download_sets_attachment_and_private_cache() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, _b) = send(
        &app,
        build(Method::GET, "/media/aaa111.mp3?download=1", &[]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        hv(&headers, "content-disposition"),
        "attachment; filename=\"Alpha-2026-11-23-1100.mp3\""
    );
    assert_eq!(hv(&headers, "cache-control"), "private, no-transform");
}

#[tokio::test]
async fn extension_must_match_format() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, _h, _b) = send(&app, build(Method::GET, "/media/aaa111.flac", &[])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // No dot → 404 as well.
    let (status, _h, _b) = send(&app, build(Method::GET, "/media/aaa111", &[])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(mock.requests(), 0);
}

#[tokio::test]
async fn hidden_and_unknown_are_identical_404() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (unknown_status, _h, unknown) =
        send(&app, build(Method::GET, "/media/does-not-exist.mp3", &[])).await;
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);

    let (status, _h, hidden) = send(&app, build(Method::GET, "/media/hidden1.mp3", &[])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(hidden, unknown, "hidden body must equal unknown body");

    let value: Value = serde_json::from_slice(&unknown).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
    assert_eq!(value["error"]["message"], "tape not found");
}

#[tokio::test]
async fn upstream_failure_is_502_storage() {
    let (_dir, app) = setup(&dead_upstream(), SIZE as i64).await;

    let (status, headers, body) = send(&app, build(Method::GET, "/media/aaa111.mp3", &[])).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(hv(&headers, "retry-after"), "5");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["error"]["code"], "storage");
}

#[tokio::test]
async fn upstream_416_is_normalized() {
    // The upstream only holds 1024 bytes, but the tape claims 8192: a range the
    // parser deems satisfiable is rejected upstream and must surface as our 416.
    let mock = start_mock(1024).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, _b) = send(
        &app,
        build(
            Method::GET,
            "/media/aaa111.mp3",
            &[("range", "bytes=5000-6000")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(hv(&headers, "content-range"), format!("bytes */{SIZE}"));
}

#[tokio::test]
async fn options_and_wrong_method() {
    let mock = start_mock(SIZE).await;
    let (_dir, app) = setup(&mock.url, SIZE as i64).await;

    let (status, headers, _b) = send(&app, build(Method::OPTIONS, "/media/aaa111.mp3", &[])).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(hv(&headers, "access-control-allow-origin"), "*");
    assert_eq!(
        hv(&headers, "access-control-allow-methods"),
        "GET, HEAD, OPTIONS"
    );

    let (status, headers, _b) = send(&app, build(Method::POST, "/media/aaa111.mp3", &[])).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let allow = hv(&headers, "allow");
    assert!(allow.contains("GET"), "Allow header: {allow}");
    assert!(allow.contains("HEAD"), "Allow header: {allow}");
    assert!(allow.contains("OPTIONS"), "Allow header: {allow}");
}
