//! End-to-end schedule/broadcast flows through `api::router` (Issue #11).
//!
//! Complementary to `schedule_expand.rs`, this file drives the *whole* cycle
//! from HTTP: create a show with a recurrence, expand it, read the merged
//! window, then run the broadcast status transitions (`start`/`end`) on a
//! persisted occurrence. It also pins DST behaviour of expansion (spring
//! forward 2026-03-29 and fall back 2026-10-25, Europe/Berlin) at the router
//! boundary, where off-by-one errors would actually reach a client.

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
async fn create_show(app: &Router, slug: &str, name: &str, recurrence: &str) -> String {
    let (status, body) = send(
        app,
        Method::POST,
        "/api/v1/shows",
        Some(json!({
            "slug": slug,
            "name": name,
            "created_by": CASTER_A,
            "caster_id": CASTER_A,
            "recurrence": recurrence,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create show: {body}");
    body["id"].as_str().expect("show id").to_string()
}

fn starts(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["starts_at"].as_str().unwrap().to_string())
        .collect()
}

/// Complete cycle: show (with recurrence) → expand → persisted broadcasts →
/// merged window → `start`/`end` transitions, all through the HTTP router.
#[tokio::test]
async fn full_cycle_create_expand_persist_start_end() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());

    create_show(&app, "weekly", "Weekly Show", WEEKLY_RULE).await;

    // Expansion persists the occurrences.
    let expand_uri = format!("/api/v1/schedule/expand?{WINDOW}");
    let (status, first) = send(&app, Method::POST, &expand_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["created"], 4);
    assert_eq!(first["skipped"], 0);

    // Re-running the same window creates nothing new (idempotent).
    let (status, second) = send(&app, Method::POST, &expand_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["created"], 0);
    assert_eq!(second["skipped"], 4);

    // The merged window lists them as persisted, with concrete ids.
    let (status, window) = send(
        &app,
        Method::GET,
        &format!("/api/v1/schedule?{WINDOW}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{window}");
    let entries = window.as_array().unwrap();
    assert_eq!(
        entries.len(),
        4,
        "no duplicates after two expands: {window}"
    );
    for entry in entries {
        assert_eq!(entry["persisted"], true);
        assert!(entry["id"].is_string(), "persisted entry needs an id");
    }

    // Repeated GET stays stable (no duplicate insertion side effects).
    let (_, window_again) = send(
        &app,
        Method::GET,
        &format!("/api/v1/schedule?{WINDOW}"),
        None,
    )
    .await;
    assert_eq!(starts(&window_again), starts(&window));

    let db_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broadcasts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_count, 4, "idempotent expand must not duplicate rows");

    // Drive one persisted occurrence through start → end via the API.
    let id = entries[0]["id"].as_str().unwrap().to_string();

    let (status, started) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{id}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["id"], id);
    assert_eq!(started["status"], "live");

    let (status, ended) = send(
        &app,
        Method::POST,
        &format!("/api/v1/broadcasts/{id}/end"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ended}");
    assert_eq!(ended["status"], "ended");
    assert!(
        ended["ends_at"].is_string(),
        "end must stamp ends_at: {ended}"
    );

    // The final state is readable back and both transitions were audited.
    let (status, fetched) =
        send(&app, Method::GET, &format!("/api/v1/broadcasts/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched["status"], "ended");
    assert!(fetched["ends_at"].is_string());

    let transitions: Vec<String> = sqlx::query_scalar(
        "SELECT event FROM event_log WHERE broadcast_id = ? AND event IN \
         ('broadcast_started', 'broadcast_ended') ORDER BY occurred_at, id",
    )
    .bind(&id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        transitions,
        vec!["broadcast_started", "broadcast_ended"],
        "audit trail for the lifecycle"
    );
}

/// Expansion across the spring-forward transition (2026-03-29) must keep the
/// Berlin wall clock (20:00) and flip the offset exactly at the gap.
#[tokio::test]
async fn expand_across_spring_forward_keeps_berlin_wall_clock() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());

    create_show(
        &app,
        "spring",
        "Spring Show",
        "DTSTART=20260327T200000;FREQ=DAILY;DURATION=PT2H",
    )
    .await;

    let (status, result) = send(
        &app,
        Method::POST,
        "/api/v1/schedule/expand?from=2026-03-27T00:00:00&to=2026-03-31T00:00:00",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["created"], 4);

    let (status, window) = send(
        &app,
        Method::GET,
        "/api/v1/schedule?from=2026-03-27T00:00:00&to=2026-03-31T00:00:00",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{window}");
    // Four consecutive days, wall clock unchanged; offset jumps +01:00 → +02:00
    // on 03-29 — no day skipped, no off-by-one at the gap.
    assert_eq!(
        starts(&window),
        vec![
            "2026-03-27T20:00:00+01:00",
            "2026-03-28T20:00:00+01:00",
            "2026-03-29T20:00:00+02:00",
            "2026-03-30T20:00:00+02:00",
        ]
    );

    // Storage is UTC and shifts by one hour exactly at the transition.
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT starts_at FROM broadcasts ORDER BY starts_at")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        vec![
            "2026-03-27T19:00:00.000Z",
            "2026-03-28T19:00:00.000Z",
            "2026-03-29T18:00:00.000Z",
            "2026-03-30T18:00:00.000Z",
        ]
    );

    let ends: Vec<String> = sqlx::query_scalar("SELECT ends_at FROM broadcasts ORDER BY starts_at")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        ends,
        vec![
            "2026-03-27T21:00:00.000Z",
            "2026-03-28T21:00:00.000Z",
            "2026-03-29T20:00:00.000Z",
            "2026-03-30T20:00:00.000Z",
        ]
    );
}

/// Expansion across the fall-back transition (2026-10-25) must keep the Berlin
/// wall clock (20:00) and flip the offset exactly at the overlap.
#[tokio::test]
async fn expand_across_fall_back_keeps_berlin_wall_clock() {
    let (_dir, pool) = setup().await;
    caster(&pool, CASTER_A).await;
    let app = app(pool.clone());

    create_show(
        &app,
        "fall",
        "Fall Show",
        "DTSTART=20261024T200000;FREQ=DAILY;DURATION=PT2H",
    )
    .await;

    let (status, result) = send(
        &app,
        Method::POST,
        "/api/v1/schedule/expand?from=2026-10-24T00:00:00&to=2026-10-28T00:00:00",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["created"], 4);

    let (status, window) = send(
        &app,
        Method::GET,
        "/api/v1/schedule?from=2026-10-24T00:00:00&to=2026-10-28T00:00:00",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{window}");
    // Offset falls back +02:00 → +01:00 on 10-25; the wall clock is stable.
    assert_eq!(
        starts(&window),
        vec![
            "2026-10-24T20:00:00+02:00",
            "2026-10-25T20:00:00+01:00",
            "2026-10-26T20:00:00+01:00",
            "2026-10-27T20:00:00+01:00",
        ]
    );

    let stored: Vec<String> =
        sqlx::query_scalar("SELECT starts_at FROM broadcasts ORDER BY starts_at")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        vec![
            "2026-10-24T18:00:00.000Z",
            "2026-10-25T19:00:00.000Z",
            "2026-10-26T19:00:00.000Z",
            "2026-10-27T19:00:00.000Z",
        ]
    );
}
