//! End-to-end chain test for Issue #12: the *whole* path an operator takes,
//! from HTTP mutation to the resulting priority selection and persistence.
//!
//! Unlike `priority_selection.rs` (which mutates via the store directly), this
//! file drives the HTTP API through `tower::ServiceExt::oneshot` and only then
//! feeds the DB-backed `list_enabled` result into the pure selector — proving
//! that a `PATCH` over the wire actually changes which layer is active.
//!
//! Chain: `GET /sources` → `PATCH {live: enabled=false}` →
//! `list_enabled` + `select_active` picks `show` → `GET /sources/{live}`
//! confirms the change persisted.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::dto::SourceLayer;
use zukka_scheduler::priority::{active_layer, select_active};
use zukka_scheduler::store;

// Fixed seed id from `0001_init.sql`.
const LIVE: &str = "0192f0a1-0002-7000-8000-000000000001";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(value) => {
            builder = builder.header("content-type", mime::APPLICATION_JSON.as_ref());
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

#[tokio::test]
async fn http_patch_toggles_active_layer_and_persists() {
    let (_dir, pool) = setup().await;
    let app = zukka_scheduler::api::router(pool.clone());

    // 1. Baseline read over HTTP: six layers, `live` on top, all enabled.
    let (status, body) = send(&app, Method::GET, "/api/v1/sources", None).await;
    assert_eq!(status, StatusCode::OK, "list: {body}");
    let layers = body.as_array().expect("array");
    assert_eq!(layers.len(), 6, "six fixed layers (C77)");
    assert_eq!(layers[0]["name"], "live");
    assert_eq!(layers[0]["priority"], 100);

    // Baseline selection from the DB: highest enabled+available layer is live.
    let sources = store::sources::list_enabled(&pool).await.expect("list");
    assert_eq!(sources.len(), 6);
    assert_eq!(active_layer(&sources, |_| true), Some(SourceLayer::Live));

    // 2. Disable `live` through the *HTTP* API (the operator path).
    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{LIVE}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(patched["enabled"], json!(false));
    assert_eq!(patched["priority"], 100, "priority immutable (C78)");

    // 3. The DB-backed selection now skips `live` and falls to `show`.
    let sources = store::sources::list_enabled(&pool).await.expect("list");
    assert_eq!(sources.len(), 5, "disabled layer excluded from selection");
    let picked = select_active(&sources, |_| true).expect("a layer");
    assert_eq!(picked.name, SourceLayer::Show);
    assert_eq!(picked.priority, 80);

    // 4. Persistence confirmed by a fresh HTTP read of the same resource and
    //    of the full list (the change survives across requests).
    let (status, after) = send(&app, Method::GET, &format!("/api/v1/sources/{LIVE}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["enabled"], json!(false));
    assert_eq!(after["priority"], 100);

    let (status, list) = send(&app, Method::GET, "/api/v1/sources", None).await;
    assert_eq!(status, StatusCode::OK);
    let live = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "live")
        .expect("live entry");
    assert_eq!(live["enabled"], json!(false));
    assert_eq!(
        live["priority"], 100,
        "list still reports immutable priority"
    );

    // 5. Re-enabling via HTTP restores the original selection.
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{LIVE}"),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sources = store::sources::list_enabled(&pool).await.expect("list");
    assert_eq!(sources.len(), 6);
    assert_eq!(active_layer(&sources, |_| true), Some(SourceLayer::Live));
}
