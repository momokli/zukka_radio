//! Integration tests for schedule merge + expansion (Issue #11).

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";
const CASTER_A: &str = "0192aaaa-0001-7000-8000-00000000000a";
const SHOW_A: &str = "0192a0a0-0001-7000-8000-0000000000a0";
const SHOW_COUNT: &str = "0192c0c0-0001-7000-8000-0000000000c0";
const SHOW_UNTIL: &str = "0192d0d0-0001-7000-8000-0000000000d0";

/// Mondays + Wednesdays at 20:00 Berlin, two hours long.
const WEEKLY_RULE: &str =
    "DTSTART=20261102T200000;FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;DURATION=PT2H";
const WINDOW: &str = "from=2026-11-02T00:00:00&to=2026-11-16T00:00:00";

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
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("user_{}", id.replace('-', "")))
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
}

async fn show(pool: &SqlitePool, id: &str, slug: &str, name: &str, recurrence: &str) {
    sqlx::query(
        "INSERT INTO shows \
         (id, slug, name, description, links, recurrence, caster_id, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, '', '[]', ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(name)
    .bind(recurrence)
    .bind(CASTER_A)
    .bind(CASTER_A)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert show");
}

async fn send(app: &Router, method: Method, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
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

async fn seed_weekly(pool: &SqlitePool) {
    caster(pool, CASTER_A).await;
    show(pool, SHOW_A, "weekly", "Weekly Show", WEEKLY_RULE).await;
}

#[tokio::test]
async fn merged_window_expands_weekly_rule() {
    let (_dir, pool) = setup().await;
    seed_weekly(&pool).await;
    let app = app(pool);

    let (status, body) = send(&app, Method::GET, &format!("/api/v1/schedule?{WINDOW}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body.as_array().expect("array");
    assert_eq!(entries.len(), 4, "Mon 2/9 + Wed 4/11: {entries:?}");

    let starts: Vec<&str> = entries
        .iter()
        .map(|entry| entry["starts_at"].as_str().unwrap())
        .collect();
    assert_eq!(
        starts,
        vec![
            "2026-11-02T20:00:00+01:00",
            "2026-11-04T20:00:00+01:00",
            "2026-11-09T20:00:00+01:00",
            "2026-11-11T20:00:00+01:00",
        ]
    );
    for entry in entries {
        assert_eq!(entry["persisted"], false);
        assert_eq!(entry["kind"], "live");
        assert_eq!(entry["status"], "scheduled");
        assert_eq!(entry["show_id"], SHOW_A);
        let ends_at = entry["ends_at"].as_str().expect("ends_at");
        assert!(
            ends_at.ends_with("T22:00:00+01:00"),
            "duration PT2H must end at 22:00 Berlin, got {ends_at}"
        );
        assert!(entry["warnings"].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
async fn expand_is_idempotent() {
    let (_dir, pool) = setup().await;
    seed_weekly(&pool).await;
    let app = app(pool.clone());

    let uri = format!("/api/v1/schedule/expand?{WINDOW}");
    let (status, first) = send(&app, Method::POST, &uri).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["created"], 4);
    assert_eq!(first["skipped"], 0);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broadcasts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 4);

    let (status, second) = send(&app, Method::POST, &uri).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["created"], 0);
    assert_eq!(second["skipped"], 4);

    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broadcasts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count_after, 4, "second expand must not insert rows");

    // Stored broadcasts are Berlin 20:00 slots with the RRULE duration.
    let stored: (String, Option<String>) =
        sqlx::query_as("SELECT starts_at, ends_at FROM broadcasts ORDER BY starts_at LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, "2026-11-02T19:00:00.000Z");
    assert_eq!(stored.1.as_deref(), Some("2026-11-02T21:00:00.000Z"));

    // Every insert wrote exactly one append-only audit row.
    let logged: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_log WHERE event = 'broadcast_created'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(logged, 4);

    // After expansion the merged view dedups: still 4 entries, now persisted.
    let (status, body) = send(&app, Method::GET, &format!("/api/v1/schedule?{WINDOW}")).await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 4, "dedup keeps one entry per slot");
    for entry in entries {
        assert_eq!(entry["persisted"], true);
        assert!(entry["id"].is_string());
    }
}

#[tokio::test]
async fn window_parameters_are_validated() {
    let (_dir, pool) = setup().await;
    let app = app(pool);

    let (status, body) = send(&app, Method::GET, "/api/v1/schedule?to=2026-11-16T00:00:00").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request");

    let (status, body) = send(
        &app,
        Method::GET,
        "/api/v1/schedule?from=2026-11-16T00:00:00&to=2026-11-02T00:00:00",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_error");

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/v1/schedule/expand?from=2026-11-16T00:00:00&to=2026-11-02T00:00:00",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn count_and_until_bound_occurrences() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    show(
        &pool,
        SHOW_COUNT,
        "count",
        "Count Show",
        "DTSTART=20261102T200000;FREQ=DAILY;COUNT=3;DURATION=PT1H",
    )
    .await;
    show(
        &pool,
        SHOW_UNTIL,
        "until",
        "Until Show",
        "DTSTART=20261102T200000;FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261109T200000",
    )
    .await;
    let app = app(pool);

    let (status, body) = send(&app, Method::GET, &format!("/api/v1/schedule?{WINDOW}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body.as_array().unwrap();

    let count_entries: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["show_id"] == SHOW_COUNT)
        .collect();
    assert_eq!(count_entries.len(), 3, "COUNT=3 caps the rule");

    let until_entries: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["show_id"] == SHOW_UNTIL)
        .collect();
    assert_eq!(until_entries.len(), 3, "UNTIL=Nov 9 inclusive: Nov 2, 4, 9");

    // Open slot: no DURATION → ends_at stays null.
    for entry in until_entries {
        assert!(entry["ends_at"].is_null());
    }
}
