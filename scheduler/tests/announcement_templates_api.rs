//! Integration tests for the announcement-template API (Issue #34).
//!
//! Like `shows_crud.rs`: requests go through `tower::ServiceExt::oneshot`
//! against `zukka_scheduler::api::router`.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER_A: &str = "0192aaaa-0001-7000-8000-00000000000a";
const MISSING_ID: &str = "0192eeee-0001-7000-8000-00000000000e";

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

async fn caster(pool: &SqlitePool, id: &str) {
    let username = format!("user_{}", id.replace('-', ""));
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(id)
    .bind(username)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
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

/// Creates a show through the API and returns its id.
async fn create_show(app: &Router, slug: &str) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({ "slug": slug, "name": slug, "created_by": CASTER_A })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create show {slug}: {body}");
    body["id"].as_str().unwrap().to_string()
}

async fn create_template(app: &Router, body: Value) -> (StatusCode, Value) {
    send(
        app,
        Method::POST,
        "/api/v1/announcement-templates",
        Some(body),
    )
    .await
}

#[tokio::test]
async fn create_global_and_get() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = create_template(
        &app,
        json!({ "name": "Live On", "event": "live_on", "body": "Wir sind live!" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    assert_eq!(body["name"], "Live On");
    assert_eq!(body["event"], "live_on");
    assert_eq!(body["channel"], "telegram");
    assert_eq!(body["enabled"], true);
    assert!(body["show_id"].is_null());
    let id = body["id"].as_str().unwrap().to_string();

    let (status, got) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcement-templates/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["id"], id);
    assert_eq!(got["channel"], "telegram");
}

#[tokio::test]
async fn duplicate_global_conflicts() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, _) = create_template(
        &app,
        json!({ "name": "One", "event": "live_on", "body": "a" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = create_template(
        &app,
        json!({ "name": "Two", "event": "live_on", "body": "b" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_template");
    assert_eq!(body["error"]["details"]["event"], "live_on");
    assert_eq!(body["error"]["details"]["channel"], "telegram");
}

#[tokio::test]
async fn same_event_global_and_per_show_ok() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let show_a = create_show(&app, "show-a").await;
    let show_b = create_show(&app, "show-b").await;

    let (status, _) = create_template(
        &app,
        json!({ "name": "Global", "event": "live_on", "body": "global" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    for show_id in [&show_a, &show_b] {
        let (status, body) = create_template(
            &app,
            json!({ "name": "Per Show", "event": "live_on", "show_id": show_id, "body": "x" }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "per show {show_id}: {body}");
        assert_eq!(body["show_id"], show_id.as_str());
    }
}

#[tokio::test]
async fn duplicate_per_show_conflicts() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let show_a = create_show(&app, "show-a").await;

    let (status, _) = create_template(
        &app,
        json!({ "name": "One", "event": "live_on", "show_id": show_a, "body": "a" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = create_template(
        &app,
        json!({ "name": "Two", "event": "live_on", "show_id": show_a, "body": "b" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_template");
}

#[tokio::test]
async fn unknown_event_is_422() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) =
        create_template(&app, json!({ "name": "X", "event": "nope", "body": "x" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "event");
}

#[tokio::test]
async fn unknown_channel_is_422() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = create_template(
        &app,
        json!({ "name": "X", "event": "live_on", "channel": "sms", "body": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "channel");
}

#[tokio::test]
async fn blank_name_and_blank_body_are_422() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = create_template(
        &app,
        json!({ "name": "   ", "event": "live_on", "body": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "name");

    let (status, body) = create_template(
        &app,
        json!({ "name": "ok", "event": "live_on", "body": "  " }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "body");
}

#[tokio::test]
async fn nonexistent_show_is_404() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = create_template(
        &app,
        json!({ "name": "X", "event": "live_on", "show_id": MISSING_ID, "body": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");

    // PATCH to a nonexistent show is also 404.
    let (_, tpl) = create_template(
        &app,
        json!({ "name": "Y", "event": "live_on", "body": "y" }),
    )
    .await;
    let id = tpl["id"].as_str().unwrap();
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/announcement-templates/{id}"),
        Some(json!({ "show_id": MISSING_ID })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn patch_body_and_enabled() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (_, tpl) = create_template(
        &app,
        json!({ "name": "X", "event": "live_on", "body": "old" }),
    )
    .await;
    let id = tpl["id"].as_str().unwrap();

    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/announcement-templates/{id}"),
        Some(json!({ "body": "new body", "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["body"], "new body");
    assert_eq!(patched["enabled"], false);
    assert_eq!(patched["name"], "X");
}

#[tokio::test]
async fn patch_show_id_null_makes_global() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let show_a = create_show(&app, "show-a").await;

    // A pre-existing global template occupies (live_on, telegram, NULL).
    let (status, _) = create_template(
        &app,
        json!({ "name": "Global", "event": "live_on", "body": "g" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, per_show) = create_template(
        &app,
        json!({ "name": "Per Show", "event": "live_on", "show_id": show_a, "body": "p" }),
    )
    .await;
    let id = per_show["id"].as_str().unwrap();

    // Making it global collides with the existing global template → 409.
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/announcement-templates/{id}"),
        Some(json!({ "show_id": null })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_template");
}

#[tokio::test]
async fn delete_then_404() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (_, tpl) =
        create_template(&app, json!({ "name": "X", "event": "manual", "body": "x" })).await;
    let id = tpl["id"].as_str().unwrap();

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/announcement-templates/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcement-templates/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/announcement-templates/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_filters() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);
    let show_a = create_show(&app, "show-a").await;

    create_template(
        &app,
        json!({ "name": "G1", "event": "live_on", "body": "a" }),
    )
    .await;
    create_template(&app, json!({ "name": "G2", "event": "error", "body": "b" })).await;
    create_template(
        &app,
        json!({ "name": "S1", "event": "live_on", "show_id": show_a, "body": "c" }),
    )
    .await;

    let (status, all) = send(&app, Method::GET, "/api/v1/announcement-templates", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all.as_array().unwrap().len(), 3);

    let (status, by_event) = send(
        &app,
        Method::GET,
        "/api/v1/announcement-templates?event=live_on",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_event.as_array().unwrap().len(), 2);

    let (status, by_show) = send(
        &app,
        Method::GET,
        &format!("/api/v1/announcement-templates?show_id={show_a}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_show.as_array().unwrap().len(), 1);
    assert_eq!(by_show[0]["name"], "S1", "unexpected: {by_show}");
}

#[tokio::test]
async fn render_preview() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (_, tpl) = create_template(
        &app,
        json!({
            "name": "Greeting",
            "event": "live_on",
            "body": "Hallo {caster_name}, {show_name} ist live! {missing}"
        }),
    )
    .await;
    let id = tpl["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcement-templates/{id}/render"),
        Some(json!({ "payload": { "caster_name": "Momo", "show_name": "Morning" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rendered"], "Hallo Momo, Morning ist live! {missing}");
    assert_eq!(body["unknown_placeholders"], json!(["missing"]));

    // Unknown template id → 404.
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/announcement-templates/{MISSING_ID}/render"),
        Some(json!({ "payload": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
