//! Issue #46 — FK integrity is preserved after apply (Story 7e).

use std::path::PathBuf;

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_scheduler::migrate::{Config, run};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

async fn setup() -> (TempDir, PathBuf, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, path, pool)
}

#[tokio::test]
async fn foreign_key_check_is_empty_after_apply() {
    let (_dir, path, pool) = setup().await;

    run(Config {
        legacy: fixture("legacy_export.json"),
        db: path,
        apply: true,
    })
    .await
    .expect("apply");

    let fk_rows = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .expect("foreign_key_check");
    assert!(fk_rows.is_empty(), "foreign_key_check must be empty");

    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .expect("integrity_check");
    assert_eq!(integrity, "ok");

    // Every tape and track resolves to an existing caster.
    let orphan_tapes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tapes t \
         LEFT JOIN casters c ON c.id = t.caster_id WHERE c.id IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(orphan_tapes, 0);

    let orphan_tracks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tracks t \
         LEFT JOIN casters c ON c.id = t.caster_id \
         WHERE t.caster_id IS NOT NULL AND c.id IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(orphan_tracks, 0);
}
