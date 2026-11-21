//! Issue #46 — orphans/duplicates abort with a full list, no writes (Story 7c).

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

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn orphans_abort_dry_run_with_full_list() {
    let (_dir, path, pool) = setup().await;

    let err = run(Config {
        legacy: fixture("legacy_export_orphans.json"),
        db: path.clone(),
        apply: false,
    })
    .await
    .unwrap_err();

    match err {
        MigrateError::Failed(issues) => {
            assert!(issues.len() >= 2, "expected >= 2 issues: {issues:?}");
            assert!(
                issues
                    .iter()
                    .any(|i| i.entity == "track" && i.reason.contains("unknown caster")),
                "track orphan missing: {issues:?}"
            );
            assert!(
                issues
                    .iter()
                    .any(|i| i.entity == "tape" && i.reason.contains("orphan")),
                "tape orphan missing: {issues:?}"
            );
        }
        other => panic!("expected MigrateError::Failed, got {other:?}"),
    }

    // Nothing was written.
    assert_eq!(count(&pool, "casters").await, 0);
    assert_eq!(count(&pool, "tracks").await, 0);
    assert_eq!(count(&pool, "tapes").await, 0);
}

#[tokio::test]
async fn orphans_abort_apply_without_writes_or_backup() {
    let (_dir, path, pool) = setup().await;

    let err = run(Config {
        legacy: fixture("legacy_export_orphans.json"),
        db: path.clone(),
        apply: true,
    })
    .await
    .unwrap_err();

    assert_eq!(err.exit_code(), 1);
    assert_eq!(count(&pool, "casters").await, 0);
    assert_eq!(count(&pool, "tracks").await, 0);
    assert_eq!(count(&pool, "tapes").await, 0);
    assert!(
        !PathBuf::from(format!("{}.bak", path.display())).exists(),
        "no backup may be created when validation aborts"
    );
}

#[tokio::test]
async fn duplicate_natural_keys_abort() {
    let (_dir, path, _pool) = setup().await;

    let err = run(Config {
        legacy: fixture("legacy_export_dupes.json"),
        db: path,
        apply: false,
    })
    .await
    .unwrap_err();

    match err {
        MigrateError::Failed(issues) => {
            // duplicate username + duplicate music_api_id + duplicate slug
            assert!(issues.len() >= 3, "expected >= 3 issues: {issues:?}");
            assert!(issues.iter().any(|i| i.entity == "caster"));
            assert!(issues.iter().any(|i| i.entity == "track"));
            assert!(issues.iter().any(|i| i.entity == "tape"));
        }
        other => panic!("expected MigrateError::Failed, got {other:?}"),
    }
}
