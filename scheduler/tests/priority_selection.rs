//! Integration tests for priority-layer selection against the real database
//! (Issue #12).
//!
//! The pure algorithm is covered by `src/priority.rs` unit tests; this file
//! wires the store (`list_enabled`) together with `priority::select_active`.

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_scheduler::dto::{SourceLayer, UpdateSourceRequest};
use zukka_scheduler::priority::{active_layer, select_active};
use zukka_scheduler::store;

const LIVE: &str = "0192f0a1-0002-7000-8000-000000000001";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

#[tokio::test]
async fn db_backed_selection_follows_priority_chain() {
    let (_dir, pool) = setup().await;

    // All layers available → highest priority (`live`).
    let sources = store::sources::list_enabled(&pool).await.expect("list");
    assert_eq!(sources.len(), 6);
    assert_eq!(active_layer(&sources, |_| true), Some(SourceLayer::Live));

    // `live` unavailable → `show` wins.
    let layer = active_layer(&sources, |layer| layer != SourceLayer::Live);
    assert_eq!(layer, Some(SourceLayer::Show));

    // Only rotation/fun up → rotation (higher priority) remains.
    let layer = active_layer(&sources, |layer| {
        matches!(layer, SourceLayer::Rotation | SourceLayer::Fun)
    });
    assert_eq!(layer, Some(SourceLayer::Rotation));
}

#[tokio::test]
async fn disabling_top_layer_changes_active_after_patch() {
    let (_dir, pool) = setup().await;

    let sources = store::sources::list_enabled(&pool).await.expect("list");
    assert_eq!(active_layer(&sources, |_| true), Some(SourceLayer::Live));

    // Disable `live` through the store (the only mutation path).
    let req = UpdateSourceRequest {
        enabled: Some(serde_json::Value::Bool(false)),
        config: None,
    };
    let patched = store::sources::update(&pool, LIVE, &req)
        .await
        .expect("patch");
    assert!(!patched.enabled);
    assert_eq!(patched.priority, 100, "priority immutable (C78)");

    // `list_enabled` no longer contains live → show is now the active layer.
    let sources = store::sources::list_enabled(&pool).await.expect("list");
    assert_eq!(sources.len(), 5);
    let picked = select_active(&sources, |_| true).expect("a layer");
    assert_eq!(picked.name, SourceLayer::Show);
    assert_eq!(picked.priority, 80);
}
