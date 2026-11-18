//! Integration tests for the queue CRUD API (Issue #10).
//!
//! Everything runs through `tower::ServiceExt::oneshot` against
//! `zukka_scheduler::api::router` — no network sockets.

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
const T1: &str = "2026-11-18T12:00:00.000Z";
const T2: &str = "2026-11-18T13:00:00.000Z";

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

async fn caster(pool: &SqlitePool, id: &str) -> String {
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
    id.to_string()
}

async fn track(pool: &SqlitePool, id: &str, music_api_id: &str) {
    sqlx::query(
        "INSERT INTO tracks (id, music_api_id, title, duration_ms, storage_key, created_at, updated_at) \
         VALUES (?, ?, 'Track', 1000, 's3/key', ?, ?)",
    )
    .bind(id)
    .bind(music_api_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert track");
}

async fn tape(pool: &SqlitePool, id: &str, slug: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 'live', ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(caster_id)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");
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

/// Creates a caster + a queue and returns `(caster_id, queue_id)`.
async fn seed_queue(app: &Router, pool: &SqlitePool, caster_id: &str) -> String {
    caster(pool, caster_id).await;
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/queues",
        Some(json!({ "caster_id": caster_id, "name": "main" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seed queue: {body}");
    body["id"].as_str().expect("queue id").to_string()
}

fn item_ids(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Queue CRUD happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_crud_happy_path() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = "c0000000-0000-7000-8000-000000000001";
    caster(&pool, caster_id).await;

    // healthz + create
    let (status, body) = send(&app, Method::GET, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Null);

    let (status, created) = send(
        &app,
        Method::POST,
        "/api/v1/queues",
        Some(json!({ "caster_id": caster_id, "name": "main", "description": "d" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create: {created}");
    assert_eq!(created["version"], 1);
    assert_eq!(created["kind"], "background");
    let id = created["id"].as_str().unwrap();
    assert_eq!(id.len(), 36, "UUIDv7 id: {id}");
    assert_eq!(created["created_at"], created["updated_at"]);

    // list contains it
    let (status, list) = send(
        &app,
        Method::GET,
        &format!("/api/v1/queues?caster_id={caster_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    // detail
    let (status, detail) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["id"], id);

    // patch bumps version
    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/queues/{id}"),
        Some(json!({ "name": "renamed", "version": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {patched}");
    assert_eq!(patched["version"], 2);
    assert_eq!(patched["name"], "renamed");

    // delete
    let (status, _) = send(&app, Method::DELETE, &format!("/api/v1/queues/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 2. Optimistic lock conflicts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn version_conflict_on_patch_and_reorder() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let id = seed_queue(&app, &pool, "c0000000-0000-7000-8000-000000000001").await;

    // first patch wins
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/queues/{id}"),
        Some(json!({ "name": "v2", "version": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // stale version
    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/queues/{id}"),
        Some(json!({ "name": "v3", "version": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "version_conflict");

    // reorder with a wrong version
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/reorder"),
        Some(json!({ "item_ids": [], "version": 99 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "version_conflict");
}

// ---------------------------------------------------------------------------
// 3. Validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn validation_errors() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = "c0000000-0000-7000-8000-000000000001";
    caster(&pool, caster_id).await;

    // empty name
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/queues",
        Some(json!({ "caster_id": caster_id, "name": "  " })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["details"]["field"], "name");

    // bogus kind
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/queues",
        Some(json!({ "caster_id": caster_id, "name": "x", "kind": "bogus" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "kind");

    // unknown caster → 404
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/queues",
        Some(json!({ "caster_id": "ffffffff-ffff-7fff-8fff-ffffffffffff", "name": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    // item: track + tape together
    let id = seed_queue(&app, &pool, "c0000000-0000-7000-8000-000000000002").await;
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "track", "track_id": "t", "tape_id": "p" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_error");

    // item: link without link_url
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "link" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "kind");
}

// ---------------------------------------------------------------------------
// 4. Duplicates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_name_and_position() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = "c0000000-0000-7000-8000-000000000001";
    let id = seed_queue(&app, &pool, caster_id).await;

    // duplicate (caster_id, name)
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/queues",
        Some(json!({ "caster_id": caster_id, "name": "main" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "duplicate_name");

    // two items on the same position
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "link", "link_url": "https://example.com/a", "position": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "link", "link_url": "https://example.com/b", "position": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "duplicate_position");
}

// ---------------------------------------------------------------------------
// 5. Items: track/tape/link, ordering, patch, delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn items_lifecycle_and_ordering() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = "c0000000-0000-7000-8000-000000000001";
    let id = seed_queue(&app, &pool, caster_id).await;
    track(&pool, "d0000000-0000-7000-8000-000000000001", "m1").await;
    tape(
        &pool,
        "70000000-0000-7000-8000-000000000001",
        "TAPE0001",
        caster_id,
    )
    .await;

    // track appended (position auto)
    let (status, item_track) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "track", "track_id": "d0000000-0000-7000-8000-000000000001" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track: {item_track}");
    assert_eq!(item_track["position"], 0);
    assert_eq!(item_track["state"], "pending");

    // tape appended
    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "tape", "tape_id": "70000000-0000-7000-8000-000000000001" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // link appended
    let (status, item_link) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/items"),
        Some(json!({ "kind": "link", "link_url": "https://example.com/c", "link_provider": "youtube" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(item_link["link_provider"], "youtube");
    assert_eq!(item_link["position"], 2);

    // ordering by position
    let (status, items) = send(
        &app,
        Method::GET,
        &format!("/api/v1/queues/{id}/items"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let positions: Vec<i64> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);

    // patch state + title_override
    let item_id = item_track["id"].as_str().unwrap();
    let (status, patched) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/queues/{id}/items/{item_id}"),
        Some(json!({ "state": "playing", "title_override": "Now" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch item: {patched}");
    assert_eq!(patched["state"], "playing");
    assert_eq!(patched["title_override"], "Now");

    // delete
    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/queues/{id}/items/{item_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/v1/queues/{id}/items/{item_id}"),
        Some(json!({ "state": "played" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 6. Reorder
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reorder_items() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let id = seed_queue(&app, &pool, "c0000000-0000-7000-8000-000000000001").await;

    for i in 0..3 {
        let (status, _) = send(
            &app,
            Method::POST,
            &format!("/api/v1/queues/{id}/items"),
            Some(json!({ "kind": "link", "link_url": format!("https://example.com/{i}") })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (_, items) = send(
        &app,
        Method::GET,
        &format!("/api/v1/queues/{id}/items"),
        None,
    )
    .await;
    let mut ids = item_ids(&items);
    ids.reverse();

    let (_, queue) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    let version = queue["version"].as_i64().unwrap();

    let (status, reordered) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/reorder"),
        Some(json!({ "item_ids": ids, "version": version })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reorder: {reordered}");
    assert_eq!(reordered["version"], version + 1);

    let (_, items) = send(
        &app,
        Method::GET,
        &format!("/api/v1/queues/{id}/items"),
        None,
    )
    .await;
    assert_eq!(item_ids(&items), ids, "order follows item_ids");
    let positions: Vec<i64> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);

    // missing / duplicate ids → 422
    let (_, queue) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    let version = queue["version"].as_i64().unwrap();
    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/reorder"),
        Some(json!({ "item_ids": [ids[0], ids[0], ids[1]], "version": version })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"]["field"], "item_ids");
}

// ---------------------------------------------------------------------------
// 7. Clear
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clear_items() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let id = seed_queue(&app, &pool, "c0000000-0000-7000-8000-000000000001").await;

    for i in 0..2 {
        send(
            &app,
            Method::POST,
            &format!("/api/v1/queues/{id}/items"),
            Some(json!({ "kind": "link", "link_url": format!("https://example.com/{i}") })),
        )
        .await;
    }
    let (_, before) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    let version = before["version"].as_i64().unwrap();

    let (status, _) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/clear"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, items) = send(
        &app,
        Method::GET,
        &format!("/api/v1/queues/{id}/items"),
        None,
    )
    .await;
    assert!(items.as_array().unwrap().is_empty(), "items cleared");

    let (_, after) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    assert_eq!(after["version"].as_i64().unwrap(), version + 1);
}

// ---------------------------------------------------------------------------
// 8. 404 + envelope shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn not_found_envelope_for_unknown_ids_and_routes() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let caster_id = "c0000000-0000-7000-8000-000000000001";
    let id = seed_queue(&app, &pool, caster_id).await;
    let ghost = "ffffffff-ffff-7fff-8fff-ffffffffffff";

    let cases = [
        (Method::GET, format!("/api/v1/queues/{ghost}"), None),
        (
            Method::PATCH,
            format!("/api/v1/queues/{ghost}"),
            Some(json!({ "name": "x", "version": 1 })),
        ),
        (Method::DELETE, format!("/api/v1/queues/{ghost}"), None),
        (Method::GET, format!("/api/v1/queues/{ghost}/items"), None),
        (
            Method::POST,
            format!("/api/v1/queues/{ghost}/items"),
            Some(json!({ "kind": "link", "link_url": "https://example.com/x" })),
        ),
        (
            Method::PATCH,
            format!("/api/v1/queues/{id}/items/{ghost}"),
            Some(json!({ "state": "played" })),
        ),
        (
            Method::DELETE,
            format!("/api/v1/queues/{id}/items/{ghost}"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/queues/{ghost}/reorder"),
            Some(json!({ "item_ids": [], "version": 1 })),
        ),
        (Method::POST, format!("/api/v1/queues/{ghost}/clear"), None),
    ];

    for (method, uri, body) in cases {
        let (status, value) = send(&app, method.clone(), &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri} → {value}");
        assert_eq!(value["error"]["code"], "not_found", "{method} {uri}");
    }

    // unknown route uses the envelope too
    let (status, value) = send(&app, Method::GET, "/api/v1/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"]["code"], "not_found");
}

// ---------------------------------------------------------------------------
// 9. Full-chain end-to-end smoke (create → item → reorder → clear → delete)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_queue_lifecycle_smoke() {
    let (_dir, pool) = setup().await;
    let app = app(pool.clone());
    let id = seed_queue(&app, &pool, "c0000000-0000-7000-8000-0000000000aa").await;

    // freshly created queue → version 1
    let (_, q) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    let mut version = q["version"].as_i64().unwrap();
    assert_eq!(version, 1);

    // two item inserts; each content mutation bumps the queue version (1 -> 3)
    let mut ids = Vec::new();
    for i in 0..2 {
        let (status, item) = send(
            &app,
            Method::POST,
            &format!("/api/v1/queues/{id}/items"),
            Some(json!({ "kind": "link", "link_url": format!("https://example.com/{i}") })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "add item {i}");
        ids.push(item["id"].as_str().unwrap().to_string());
    }
    let (_, q) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    version = q["version"].as_i64().unwrap();
    assert_eq!(version, 3, "each item insert bumps queue version");

    // reorder consumes the version and bumps it (3 -> 4)
    ids.reverse();
    let (status, reordered) = send(
        &app,
        Method::POST,
        &format!("/api/v1/queues/{id}/reorder"),
        Some(json!({ "item_ids": ids, "version": version })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reorder: {reordered}");
    assert_eq!(reordered["version"], version + 1);

    // clear empties the queue and bumps the version (4 -> 5)
    let (status, _) = send(&app, Method::POST, &format!("/api/v1/queues/{id}/clear"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, items) = send(&app, Method::GET, &format!("/api/v1/queues/{id}/items"), None).await;
    assert!(items.as_array().unwrap().is_empty(), "items cleared");
    let (_, q) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    version = q["version"].as_i64().unwrap();
    assert_eq!(version, 5);

    // delete → 204, then every access to the id is a 404 with the envelope
    let (status, _) = send(&app, Method::DELETE, &format!("/api/v1/queues/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(&app, Method::GET, &format!("/api/v1/queues/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    assert!(body["error"]["message"].is_string(), "envelope carries a message");
}
