//! HTTP tests for `GET /metrics` (Issue #44 — Monitoring, §3.8/§4.2).
//!
//! Renders the Prometheus text exposition format through
//! `zukka_scheduler::api::router` (the main router carries `/metrics` in
//! addition to the dedicated `:9091` listener). Also covers the store-level
//! queue-fill aggregate used by `zukka_queue_fill_seconds`.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use zukka_scheduler::store::{queue_items, queues, tracks};
use zukka_scheduler::uuid::new_id;

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
        .await
        .expect("connect + migrate");
    (dir, pool)
}

fn app(pool: SqlitePool) -> Router {
    zukka_scheduler::api::router(pool)
}

async fn body_string(app: Router, uri: &str) -> (StatusCode, String, Option<String>) {
    let request = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("router response");
    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        String::from_utf8_lossy(&body).into_owned(),
        content_type,
    )
}

#[tokio::test]
async fn metrics_returns_prometheus_text() {
    let (_dir, pool) = setup().await;

    let (status, body, content_type) = body_string(app(pool), "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        content_type.as_deref(),
        Some("text/plain; version=0.0.4"),
        "unexpected content type"
    );

    // Every minimum metric from §3.8 must be present with HELP + TYPE.
    for name in [
        "zukka_icecast_listeners",
        "zukka_live_sources",
        "zukka_source_state",
        "zukka_queue_fill_seconds",
        "zukka_music_api_errors_total",
        "zukka_scheduler_db_locked_total",
        "zukka_build_info",
    ] {
        assert!(
            body.contains(&format!("# HELP {name} ")),
            "HELP missing: {name}"
        );
        assert!(
            body.contains(&format!("# TYPE {name} ")),
            "TYPE missing: {name}"
        );
    }

    // All six layers are reported (0/1).
    for layer in ["live", "show", "queue", "rotation", "relay", "fun"] {
        assert!(
            body.contains(&format!("zukka_source_state{{layer=\"{layer}\"}}")),
            "layer missing: {layer}"
        );
    }
    assert!(
        body.contains("zukka_build_info{version="),
        "build_info labels missing"
    );
}

#[tokio::test]
async fn queue_fill_sums_pending_track_durations() {
    let (_dir, pool) = setup().await;

    let caster_id = new_id();
    sqlx::query(
        "INSERT INTO casters \
         (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, ?, 'x', '0192f0a1-0001-7000-8000-000000000003', ?, ?)",
    )
    .bind(&caster_id)
    .bind("qfill")
    .bind("Q Fill")
    .bind("2026-01-01T00:00:00.000Z")
    .bind("2026-01-01T00:00:00.000Z")
    .execute(&pool)
    .await
    .expect("insert caster");

    let queue = queues::create(
        &pool,
        &zukka_scheduler::dto::CreateQueueRequest {
            caster_id: caster_id.clone(),
            name: "fill".into(),
            description: None,
            kind: None,
        },
    )
    .await
    .expect("create queue");

    let track = tracks::create(
        &pool,
        &zukka_scheduler::dto::CreateTrackRequest {
            music_api_id: "m-1".into(),
            title: "T".into(),
            storage_key: "k".into(),
            duration_ms: 90_000,
            artist: None,
            album: None,
            isrc: None,
            artwork_url: None,
            visibility: Some("shared".into()),
            caster_id: None,
            metadata: None,
        },
    )
    .await
    .expect("create track");

    queue_items::add(
        &pool,
        &queue.id,
        &zukka_scheduler::dto::CreateItemRequest {
            kind: "track".into(),
            track_id: Some(track.id.clone()),
            tape_id: None,
            link_url: None,
            link_provider: None,
            title_override: None,
            position: None,
        },
    )
    .await
    .expect("add item");

    let rows = queues::fill_seconds(&pool).await.expect("fill_seconds");
    let entry = rows
        .iter()
        .find(|(id, _)| id == &queue.id)
        .expect("queue present");
    assert!((entry.1 - 90.0).abs() < f64::EPSILON, "got {}", entry.1);
}
