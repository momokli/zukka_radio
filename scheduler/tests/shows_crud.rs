//! Integration tests for the show CRUD API (Issue #11).
//!
//! Like `queue_crud.rs`: requests go through `tower::ServiceExt::oneshot`
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
const CASTER_B: &str = "0192bbbb-0001-7000-8000-00000000000b";

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

#[tokio::test]
async fn create_show_normalises_slug_and_recurrence() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({
            "slug": "  morning-show  ",
            "name": "Morning Show",
            "created_by": CASTER_A,
            "caster_id": CASTER_A,
            "links": [{"label": "Site", "url": "https://example.org"}],
            "recurrence": "FREQ=WEEKLY;BYDAY=WE,MO;DURATION=PT2H;DTSTART=20260105T200000"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "create show: {body}");
    assert_eq!(body["slug"], "morning-show");
    assert_eq!(body["description"], "");
    assert_eq!(body["links"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["recurrence"],
        "DTSTART=20260105T200000;FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;DURATION=PT2H"
    );
    assert_eq!(body["caster_id"], CASTER_A);
    assert_eq!(body["created_by"], CASTER_A);
    assert!(body["id"].as_str().unwrap().len() == 36);
}

#[tokio::test]
async fn create_show_rejects_duplicate_slug() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    let payload = json!({ "slug": "dup", "name": "One", "created_by": CASTER_A });
    let (first, _) = send(&app, Method::POST, "/api/v1/shows", Some(payload)).await;
    assert_eq!(first, StatusCode::CREATED);

    let (second, body) = send(
        &app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({ "slug": "dup", "name": "Two", "created_by": CASTER_A })),
    )
    .await;
    assert_eq!(second, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "duplicate_slug");
}

#[tokio::test]
async fn create_show_unknown_caster_is_404() {
    let (_dir, pool) = setup().await;
    let app = app(pool);

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({
            "slug": "ghost",
            "name": "Ghost",
            "created_by": CASTER_A,
            "caster_id": "0192cccc-0001-7000-8000-00000000000c"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn create_show_invalid_recurrence_is_422() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool);

    for rule in [
        "DTSTART=20260105T200000;FREQ=SECONDLY",
        "DTSTART=20260105T200000;FREQ=DAILY;FOO=1",
        "DTSTART=20260105T200000;FREQ=DAILY;COUNT=2;UNTIL=20260201T000000",
        "FREQ=DAILY",
    ] {
        let (status, body) = send(
            &app,
            Method::POST,
            "/api/v1/shows",
            Some(json!({
                "slug": format!("s-{}", rule.len()),
                "name": "Bad Rule",
                "created_by": CASTER_A,
                "recurrence": rule
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "rule {rule}: {body}"
        );
        assert_eq!(body["error"]["code"], "validation_error");
        assert_eq!(body["error"]["details"]["field"], "recurrence");
    }
}

#[tokio::test]
async fn list_get_patch_delete_roundtrip() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    caster(&pool, CASTER_B).await;
    let app = app(pool);

    let (_, show_a) = send(
        &app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({
            "slug": "a", "name": "A", "created_by": CASTER_A, "caster_id": CASTER_A,
            "artwork_url": "https://example.org/a.png"
        })),
    )
    .await;
    let id_a = show_a["id"].as_str().unwrap().to_string();

    let (_, show_b) = send(
        &app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({ "slug": "b", "name": "B", "created_by": CASTER_B, "caster_id": CASTER_B })),
    )
    .await;
    let id_b = show_b["id"].as_str().unwrap().to_string();

    // List (all + filtered).
    let (status, all) = send(&app, Method::GET, "/api/v1/shows", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all.as_array().unwrap().len(), 2);

    let uri = format!("/api/v1/shows?caster_id={CASTER_A}");
    let (status, filtered) = send(&app, Method::GET, &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    assert_eq!(filtered[0]["id"], id_a);

    // Get one.
    let (status, one) = send(&app, Method::GET, &format!("/api/v1/shows/{id_a}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["artwork_url"], "https://example.org/a.png");

    // Patch: rename, clear artwork, set recurrence; other fields untouched.
    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/shows/{id_a}"),
        Some(json!({
            "name": "A Renamed",
            "artwork_url": null,
            "recurrence": "DTSTART=20260201T180000;FREQ=DAILY;COUNT=4"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(patched["name"], "A Renamed");
    assert!(patched["artwork_url"].is_null());
    assert_eq!(patched["slug"], "a");
    assert_eq!(
        patched["recurrence"],
        "DTSTART=20260201T180000;FREQ=DAILY;INTERVAL=1;COUNT=4"
    );

    // Delete + 404 afterwards.
    let (status, _) = send(&app, Method::DELETE, &format!("/api/v1/shows/{id_b}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&app, Method::GET, &format!("/api/v1/shows/{id_b}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn patch_unknown_show_is_404() {
    let (_dir, pool) = setup().await;
    let app = app(pool);
    let (status, body) = send(
        &app,
        Method::PATCH,
        "/api/v1/shows/0192dddd-0001-7000-8000-00000000000d",
        Some(json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
