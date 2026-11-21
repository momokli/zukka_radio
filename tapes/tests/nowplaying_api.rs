//! HTTP-level tests for `GET /api/v1/public/nowplaying` (Issue #107).
//!
//! No network: the scheduler URL is empty, so the client's request fails
//! immediately and the endpoint must still answer `200 no-store` with the
//! degraded body (`live = false`, all fields `null`). A real scheduler is
//! never contacted here.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const BASE_URL: &str = "https://tapes.test";

async fn app() -> (TempDir, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let writer = zukka_tapes::fixtures::create_db(&path)
        .await
        .expect("create db");
    writer.close().await;
    let pool = zukka_tapes::db::connect_read_only(&path)
        .await
        .expect("read-only pool");
    // Empty scheduler URL → the internal call cannot succeed (no network).
    let app = zukka_tapes::api::router(
        pool,
        BASE_URL.to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    (dir, app)
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn degrades_to_200_no_store_when_scheduler_unreachable() {
    let (_dir, app) = app().await;
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/public/nowplaying")
        .body(Body::empty())
        .expect("request");

    let (status, headers, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");

    let value: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(value["title"], Value::Null);
    assert_eq!(value["artist"], Value::Null);
    assert_eq!(value["show"], Value::Null);
    assert_eq!(value["caster"], Value::Null);
    assert_eq!(value["live"], Value::Bool(false));
    assert_eq!(value["started_at"], Value::Null);
    assert!(
        value["server_time"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
}

#[tokio::test]
async fn options_preflight_is_204_with_cors() {
    let (_dir, app) = app().await;
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/public/nowplaying")
        .body(Body::empty())
        .expect("request");

    let (status, headers, _body) = send(&app, request).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
}

#[tokio::test]
async fn unknown_api_path_stays_404_envelope() {
    let (_dir, app) = app().await;
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/public/nope")
        .body(Body::empty())
        .expect("request");

    let (status, _headers, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let value: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
}
