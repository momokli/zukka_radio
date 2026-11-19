//! Integration tests for the sources API (Issue #12).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets. The `sources` table +
//! seed are created by the frozen migration `0001_init.sql`.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

// Fixed seed ids from `0001_init.sql`.
const LIVE: &str = "0192f0a1-0002-7000-8000-000000000001";
const SHOW: &str = "0192f0a1-0002-7000-8000-000000000002";
const GHOST: &str = "ffffffff-ffff-7fff-8fff-ffffffffffff";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

/// Sends one request through the router and decodes the JSON body.
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

async fn count_sources(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sources")
        .fetch_one(pool)
        .await
        .expect("count sources")
}

// ---------------------------------------------------------------------------
// 1. Seed assertion over the API (not just SQL)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn seed_layers_via_api() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, body) = send(&app, Method::GET, "/api/v1/sources", None).await;
    assert_eq!(status, StatusCode::OK, "list: {body}");
    let layers = body.as_array().expect("array");

    let names: Vec<&str> = layers.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["live", "show", "queue", "rotation", "relay", "fun"],
        "layers sorted priority DESC, name ASC"
    );

    let priorities: Vec<i64> = layers
        .iter()
        .map(|s| s["priority"].as_i64().unwrap())
        .collect();
    assert_eq!(priorities, vec![100, 80, 60, 40, 20, 1]);

    for layer in layers {
        assert_eq!(layer["enabled"], json!(true));
        assert_eq!(layer["config"], json!({}));
        assert_eq!(layer["kind"], layer["name"], "seed sets kind == name");
        assert!(layer["id"].as_str().unwrap().len() == 36);
    }
}

// ---------------------------------------------------------------------------
// 2. GET detail + 404 envelope
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_one_and_not_found() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, body) = send(&app, Method::GET, &format!("/api/v1/sources/{LIVE}"), None).await;
    assert_eq!(status, StatusCode::OK, "detail: {body}");
    assert_eq!(body["name"], "live");
    assert_eq!(body["priority"], 100);

    let (status, body) = send(&app, Method::GET, &format!("/api/v1/sources/{GHOST}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

// ---------------------------------------------------------------------------
// 3. PATCH enabled / config (C78: priority immutable)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_enabled_persists_and_priority_unchanged() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

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
    assert_eq!(patched["name"], "live");
    assert_ne!(
        patched["updated_at"], patched["created_at"],
        "updated_at moves on patch"
    );

    // re-read confirms persistence
    let (_, after) = send(&app, Method::GET, &format!("/api/v1/sources/{LIVE}"), None).await;
    assert_eq!(after["enabled"], json!(false));

    // DB-level check: priority untouched
    let priority: i64 = sqlx::query_scalar("SELECT priority FROM sources WHERE id = ?")
        .bind(LIVE)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(priority, 100);
}

#[tokio::test]
async fn patch_config_persists_and_enabled_unchanged() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{SHOW}"),
        Some(json!({ "config": { "foo": 1 } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(patched["config"], json!({ "foo": 1 }));
    assert_eq!(patched["enabled"], json!(true), "enabled unchanged");

    // both fields in one request
    let (status, both) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{SHOW}"),
        Some(json!({ "enabled": false, "config": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(both["enabled"], json!(false));
    assert_eq!(both["config"], json!({}));
}

// ---------------------------------------------------------------------------
// 4. Immutable fields rejected (C78) + validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn immutable_fields_and_unknown_rejected() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    for payload in [
        json!({ "priority": 80 }),
        json!({ "name": "x" }),
        json!({ "kind": "x" }),
        json!({ "id": GHOST }),
        json!({ "created_at": "2026-01-01T00:00:00.000Z" }),
    ] {
        let (status, body) = send(
            &app,
            Method::PATCH,
            &format!("/api/v1/sources/{LIVE}"),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "unknown field must be 400: {payload} → {body}"
        );
        assert_eq!(body["error"]["code"], "bad_request");
    }

    // DB unchanged after all rejected attempts
    let row: (String, i64) = sqlx::query_as("SELECT name, priority FROM sources WHERE id = ?")
        .bind(LIVE)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row, ("live".to_string(), 100));
    assert_eq!(count_sources(&pool).await, 6);
}

#[tokio::test]
async fn validation_cases() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    // empty body → 422
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{LIVE}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "empty body: {body}"
    );
    assert_eq!(body["error"]["code"], "validation_error");

    // config null → 422
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{LIVE}"),
        Some(json!({ "config": null })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "config null: {body}"
    );
    assert_eq!(body["error"]["details"]["field"], "config");

    // config non-object → 422
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{LIVE}"),
        Some(json!({ "config": [1, 2] })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "config array: {body}"
    );
    assert_eq!(body["error"]["details"]["field"], "config");

    // enabled wrong type → 422 (field-level, not the extractor's 400)
    for bad in [
        json!({ "enabled": 0 }),
        json!({ "enabled": 1 }),
        json!({ "enabled": "true" }),
    ] {
        let (status, body) = send(
            &app,
            Method::PATCH,
            &format!("/api/v1/sources/{LIVE}"),
            Some(bad.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "enabled type: {bad} → {body}"
        );
        assert_eq!(body["error"]["details"]["field"], "enabled");
    }

    // unknown id → 404
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/sources/{GHOST}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    assert_eq!(count_sources(&pool).await, 6);
}

// ---------------------------------------------------------------------------
// 5. Forbidden routes (C77): no POST / DELETE / PUT
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_delete_put_are_not_registered() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/v1/sources",
        Some(json!({ "name": "live", "priority": 100 })),
    )
    .await;
    assert!(
        !status.is_success(),
        "POST /sources must not succeed, got {status}"
    );

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/sources/{LIVE}"),
        None,
    )
    .await;
    assert!(
        !status.is_success(),
        "DELETE /sources/{{id}} must not succeed, got {status}"
    );

    let (status, _) = send(
        &app,
        Method::PUT,
        &format!("/api/v1/sources/{LIVE}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert!(
        !status.is_success(),
        "PUT /sources/{{id}} must not succeed, got {status}"
    );

    // the six layers are untouched
    assert_eq!(count_sources(&pool).await, 6);
    let (status, body) = send(&app, Method::GET, "/api/v1/sources", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 6);
}
