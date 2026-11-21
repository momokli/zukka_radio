//! Issue #46 — idempotency: a second apply writes zero new rows (Story 7d).

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

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn second_apply_writes_zero_new_rows() {
    let (_dir, path, pool) = setup().await;

    let first = run(Config {
        legacy: fixture("legacy_export.json"),
        db: path.clone(),
        apply: true,
    })
    .await
    .expect("first apply");
    assert_eq!(first.report.casters.written, 3);
    assert_eq!(first.report.tracks.written, 3);
    assert_eq!(first.report.tapes.written, 2);

    let ids_after_first: Vec<String> = sqlx::query_scalar("SELECT id FROM tapes ORDER BY slug")
        .fetch_all(&pool)
        .await
        .unwrap();

    // The first apply created a backup; remove it so the second apply can back up.
    std::fs::remove_file(format!("{}.bak", path.display())).expect("remove first backup");

    let second = run(Config {
        legacy: fixture("legacy_export.json"),
        db: path.clone(),
        apply: true,
    })
    .await
    .expect("second apply");

    assert_eq!(second.report.casters.written, 0, "no new casters");
    assert_eq!(second.report.tracks.written, 0, "no new tracks");
    assert_eq!(second.report.tapes.written, 0, "no new tapes");
    assert_eq!(second.report.casters.skipped, 3);
    assert_eq!(second.report.tracks.skipped, 3);
    assert_eq!(second.report.tapes.skipped, 2);
    assert!(second.report.invariant_ok());

    // Row counts unchanged and IDs stable.
    assert_eq!(count(&pool, "casters").await, 3);
    assert_eq!(count(&pool, "tracks").await, 3);
    assert_eq!(count(&pool, "tapes").await, 2);
    let ids_after_second: Vec<String> = sqlx::query_scalar("SELECT id FROM tapes ORDER BY slug")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids_after_first, ids_after_second, "tape ids must be stable");
}
