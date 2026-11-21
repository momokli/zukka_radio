//! Issue #46 — dry-run must never write (Story 7a).

use std::path::PathBuf;

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_scheduler::migrate::{Config, MigrateError, run};

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

async fn counts(pool: &SqlitePool) -> (i64, i64, i64) {
    let casters = sqlx::query_scalar("SELECT COUNT(*) FROM casters")
        .fetch_one(pool)
        .await
        .unwrap();
    let tracks = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(pool)
        .await
        .unwrap();
    let tapes = sqlx::query_scalar("SELECT COUNT(*) FROM tapes")
        .fetch_one(pool)
        .await
        .unwrap();
    (casters, tracks, tapes)
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let (_dir, path, pool) = setup().await;
    let before = counts(&pool).await;
    assert_eq!(before, (0, 0, 0));

    let outcome = run(Config {
        legacy: fixture("legacy_export.json"),
        db: path.clone(),
        apply: false,
    })
    .await
    .expect("dry-run must succeed");

    assert!(!outcome.applied, "dry-run must not be applied");
    assert!(outcome.backup.is_none(), "dry-run must not create a backup");
    assert_eq!(outcome.report.casters.read, 3);
    assert_eq!(outcome.report.casters.written, 3);
    assert_eq!(outcome.report.tracks.read, 3);
    assert_eq!(outcome.report.tracks.written, 3);
    assert_eq!(outcome.report.tapes.read, 2);
    assert_eq!(outcome.report.tapes.written, 2);
    assert!(outcome.report.invariant_ok(), "counting invariant");

    // The database is untouched.
    assert_eq!(counts(&pool).await, before);
    assert!(
        !PathBuf::from(format!("{}.bak", path.display())).exists(),
        "dry-run must not create a backup file"
    );
}

#[tokio::test]
async fn missing_export_is_io_error_exit_2() {
    let (_dir, path, _pool) = setup().await;
    let err = run(Config {
        legacy: PathBuf::from("/nonexistent/legacy_export.json"),
        db: path,
        apply: false,
    })
    .await
    .unwrap_err();
    assert!(matches!(err, MigrateError::Io(_)), "got {err:?}");
    assert_eq!(err.exit_code(), 2);
}

#[tokio::test]
async fn malformed_json_is_validation_error_exit_1() {
    let (dir, path, _pool) = setup().await;
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let err = run(Config {
        legacy: bad,
        db: path,
        apply: false,
    })
    .await
    .unwrap_err();
    assert!(matches!(err, MigrateError::Validation(_)), "got {err:?}");
    assert_eq!(err.exit_code(), 1);
}
