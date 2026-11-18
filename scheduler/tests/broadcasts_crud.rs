//! Integration tests for the broadcast CRUD API (Issue #11).

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
const SHOW_A: &str = "0192a0a0-0001-7000-8000-0000000000a0";
const SHOW_B: &str = "0192b0b0-0001-7000-8000-0000000000b0";
const TAPE_A: &str = "0192c0c0-0001-7000-8000-0000000000c0";
const QUEUE_A: &str = "0192d0d0-0001-7000-8000-0000000000d0";
const START: &str = "2026-11-18T20:00:00+01:00";
const START_UTC: &str = "2026-11-18T19:00:00.000Z";

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

async fn show(pool: &SqlitePool, id: &str, slug: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO shows (id, slug, name, description, links, caster_id, created_by, created_at, updated_at) \
         VALUES (?, ?, 'Show', '', '[]', ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(caster_id)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert show");
}

async fn tape(pool: &SqlitePool, id: &str, slug: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 'live', ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(caster_id)
    .bind("2026-11-18T12:00:00.000Z")
    .bind("2026-11-18T13:00:00.000Z")
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");
}

async fn queue(pool: &SqlitePool, id: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO queues (id, caster_id, name, description, kind, version, created_at, updated_at) \
         VALUES (?, ?, 'main', '', 'slot', 1, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue");
}

async fn seed(pool: &SqlitePool) {
    caster(pool, CASTER_A).await;
    caster(pool, CASTER_B).await;
    show(pool, SHOW_A, "show-a", CASTER_A).await;
    show(pool, SHOW_B, "show-b", CASTER_B).await;
    tape(pool, TAPE_A, "tape-a", CASTER_A).await;
    queue(pool, QUEUE_A, CASTER_A).await;
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

async fn post_broadcast(app: &Router, payload: Value) -> (StatusCode, Value) {
    send(app, Method::POST, "/api/v1/broadcasts", Some(payload)).await
}

#[tokio::test]
async fn create_each_kind() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    // live
    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "show_id": SHOW_A, "kind": "live",
            "title": "Live Slot", "starts_at": START, "ends_at": "2026-11-18T22:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "live: {body}");
    assert_eq!(body["kind"], "live");
    assert_eq!(body["status"], "scheduled");
    assert_eq!(body["version"], 1);
    assert_eq!(body["created_by"], CASTER_A);
    assert_eq!(body["warnings"], json!([]));

    // recording
    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "recording", "title": "Rec",
            "tape_id": TAPE_A, "starts_at": "2026-11-19T10:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "recording: {body}");
    assert_eq!(body["tape_id"], TAPE_A);

    // playlist
    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "playlist", "title": "List",
            "queue_id": QUEUE_A, "starts_at": "2026-11-19T12:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "playlist: {body}");
    assert_eq!(body["queue_id"], QUEUE_A);
}

#[tokio::test]
async fn content_matrix_violations_are_422() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let cases = [
        (
            json!({
                "caster_id": CASTER_A, "kind": "live", "title": "x",
                "tape_id": TAPE_A, "starts_at": START
            }),
            "tape_id",
        ),
        (
            json!({
                "caster_id": CASTER_A, "kind": "live", "title": "x",
                "queue_id": QUEUE_A, "starts_at": START
            }),
            "queue_id",
        ),
        (
            json!({
                "caster_id": CASTER_A, "kind": "recording", "title": "x",
                "starts_at": START
            }),
            "tape_id",
        ),
        (
            json!({
                "caster_id": CASTER_A, "kind": "playlist", "title": "x",
                "starts_at": START
            }),
            "queue_id",
        ),
    ];

    for (payload, field) in cases {
        let (status, body) = post_broadcast(&app, payload.clone()).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{payload}: {body}"
        );
        assert_eq!(body["error"]["code"], "validation_error");
        assert_eq!(body["error"]["details"]["field"], field);
    }
}

#[tokio::test]
async fn time_roundtrip_and_window_validation() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool.clone());

    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "live", "title": "Time",
            "starts_at": START
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["starts_at"], START);
    assert!(body["ends_at"].is_null(), "open slot keeps NULL ends_at");

    // Stored as canonical UTC milliseconds.
    let stored: String = sqlx::query_scalar("SELECT starts_at FROM broadcasts WHERE id = ?")
        .bind(body["id"].as_str().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, START_UTC);

    // ends_at <= starts_at → 422 ends_at.
    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "live", "title": "Bad",
            "starts_at": START, "ends_at": "2026-11-18T19:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["details"]["field"], "ends_at");
}

#[tokio::test]
async fn self_overlap_is_hard_conflict() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let (status, first) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "show_id": SHOW_A, "kind": "live", "title": "First",
            "starts_at": START, "ends_at": "2026-11-18T22:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");

    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "show_id": SHOW_A, "kind": "live", "title": "Second",
            "starts_at": "2026-11-18T21:00:00+01:00", "ends_at": "2026-11-18T23:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "show_overlap");
    assert_eq!(body["error"]["details"]["show_id"], SHOW_A);
    assert_eq!(
        body["error"]["details"]["conflicting_broadcast_id"],
        first["id"]
    );
}

#[tokio::test]
async fn foreign_overlap_and_live_overlap_are_warnings() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let (status, first) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "show_id": SHOW_A, "kind": "live", "title": "First",
            "starts_at": START, "ends_at": "2026-11-18T22:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["warnings"], json!([]));

    // Different show + different caster, both live, overlapping.
    let (status, second) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_B, "show_id": SHOW_B, "kind": "live", "title": "Second",
            "starts_at": "2026-11-18T21:00:00+01:00", "ends_at": "2026-11-18T23:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let warnings = second["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings[0]["code"], "overlap");
    assert!(
        warnings
            .iter()
            .any(|warning| warning["code"] == "live_overlap"),
        "expected live_overlap in {warnings:?}"
    );
    assert_eq!(warnings[0]["broadcast_id"], first["id"]);
    assert_eq!(warnings[0]["severity"], "warning");
}

#[tokio::test]
async fn caster_double_booking_warns() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let (status, _) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "show_id": SHOW_A, "kind": "playlist", "title": "Q1",
            "queue_id": QUEUE_A, "starts_at": START, "ends_at": "2026-11-18T22:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "show_id": SHOW_B, "kind": "live", "title": "Q2",
            "starts_at": "2026-11-18T21:00:00+01:00", "ends_at": "2026-11-18T23:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let warnings = body["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|warning| warning["code"] == "caster_overlap"),
        "expected caster_overlap in {warnings:?}"
    );
}

#[tokio::test]
async fn patch_requires_matching_version() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let (status, created) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "live", "title": "Patch me", "starts_at": START
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();

    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/broadcasts/{id}"),
        Some(json!({ "title": "Nope", "version": 99 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "version_conflict");

    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/broadcasts/{id}"),
        Some(json!({ "title": "Updated", "version": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Updated");
    assert_eq!(body["version"], 2);
    assert_eq!(body["starts_at"], START);
    assert!(body["ends_at"].is_null());
}

#[tokio::test]
async fn status_transitions() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let (status, created) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "live", "title": "Run",
            "starts_at": "2026-11-18T10:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    // scheduled → live
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "live");
    assert_eq!(body["version"], 2);

    // live → live is illegal
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "illegal_transition");
    assert_eq!(body["error"]["details"]["action"], "start");

    // live → ended stamps ends_at
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{id}/end"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ended");
    assert!(!body["ends_at"].is_null(), "end must stamp ends_at");

    // cancel from scheduled works once, then illegal
    let (status, second) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "playlist", "title": "Cancel me",
            "queue_id": QUEUE_A, "starts_at": "2026-11-20T10:00:00+01:00",
            "ends_at": "2026-11-20T12:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let second_id = second["id"].as_str().unwrap();

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{second_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{second_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "illegal_transition");
}

#[tokio::test]
async fn list_filters_and_delete() {
    let (_dir, pool) = setup().await;
    seed(&pool).await;
    let app = app(pool);

    let (status, created) = post_broadcast(
        &app,
        json!({
            "caster_id": CASTER_A, "kind": "live", "title": "Listed", "starts_at": START,
            "ends_at": "2026-11-18T22:00:00+01:00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (status, list) = send(
        &app,
        Method::GET,
        "/api/v1/broadcasts?from=2026-11-18T00:00:00%2B01:00&to=2026-11-19T00:00:00%2B01:00",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, empty) = send(
        &app,
        Method::GET,
        "/api/v1/broadcasts?from=2026-12-01T00:00:00%2B01:00&to=2026-12-02T00:00:00%2B01:00",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(empty.as_array().unwrap().is_empty());

    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/broadcasts/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&app, Method::GET, &format!("/api/v1/broadcasts/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
