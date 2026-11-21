//! Issue #46 — apply counts + field assertions (Story 7b).

use std::path::PathBuf;

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_scheduler::migrate::slug::is_valid_slug;
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
async fn apply_counts_and_field_mapping() {
    let (_dir, path, pool) = setup().await;

    let outcome = run(Config {
        legacy: fixture("legacy_export.json"),
        db: path.clone(),
        apply: true,
    })
    .await
    .expect("apply must succeed");

    assert!(outcome.applied);
    let backup = outcome.backup.clone().expect("backup path");
    assert!(
        backup.exists(),
        "backup file must exist: {}",
        backup.display()
    );

    // Row counts match the fixture.
    assert_eq!(count(&pool, "casters").await, 3);
    assert_eq!(count(&pool, "tracks").await, 3);
    assert_eq!(count(&pool, "tapes").await, 2);

    // Report counts match.
    assert_eq!(outcome.report.casters.written, 3);
    assert_eq!(outcome.report.tracks.written, 3);
    assert_eq!(outcome.report.tapes.written, 2);
    assert_eq!(outcome.report.casters.skipped, 0);
    assert_eq!(outcome.report.tapes.failed, 0);
    assert!(outcome.report.invariant_ok());

    // Slugs are valid, stable 8-char Crockford base32.
    let slugs: Vec<String> = sqlx::query_scalar("SELECT slug FROM tapes")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(slugs.len(), 2);
    for slug in &slugs {
        assert_eq!(slug.len(), 8, "slug: {slug}");
        assert!(is_valid_slug(slug), "slug: {slug}");
    }

    // Every migrated caster has a locked Argon2id hash and the caster role.
    let hashes: Vec<(String, String, String)> =
        sqlx::query_as("SELECT username, password_hash, role_id FROM casters")
            .fetch_all(&pool)
            .await
            .unwrap();
    for (username, hash, role) in &hashes {
        assert!(hash.starts_with("$argon2id$"), "{username}: {hash}");
        assert_eq!(role, "0192f0a1-0001-7000-8000-000000000003");
        assert_eq!(username, &username.to_lowercase(), "username lowercase");
    }

    // DISPLAY_NAMES mapping: hovio → ANKD, mctoastus → duck, momo → momo.
    let display: Vec<(String, String)> =
        sqlx::query_as("SELECT username, display_name FROM casters ORDER BY username")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        display,
        vec![
            ("hovio".to_string(), "ANKD".to_string()),
            ("mctoastus".to_string(), "duck".to_string()),
            ("momo".to_string(), "momo".to_string()),
        ]
    );

    // is_active mirrors the legacy `aktiv` flag (mctoastus = false).
    let inactive: i64 =
        sqlx::query_scalar("SELECT is_active FROM casters WHERE username = 'mctoastus'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(inactive, 0);

    // Empty ISRC → NULL (no UNIQUE collision on empty strings).
    let isrc: Option<String> =
        sqlx::query_scalar("SELECT isrc FROM tracks WHERE music_api_id = '1003'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(isrc, None);
    let isrc: Option<String> =
        sqlx::query_scalar("SELECT isrc FROM tracks WHERE music_api_id = '1001'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(isrc.as_deref(), Some("DEZZ00000001"));

    // music_api_id = stringified legacy file id.
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT music_api_id FROM tracks ORDER BY music_api_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(ids, vec!["1001", "1002", "1003"]);

    // Tracks are shared and owned by the resolved caster.
    let shared: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE visibility = 'shared'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(shared, 3);
    let unowned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE caster_id IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unowned, 0);

    // Tapes: source live/upload, status ready, not visible, duration from diff.
    let live: String = sqlx::query_scalar("SELECT source FROM tapes WHERE music_api_id IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(live, "live");
    let upload: String =
        sqlx::query_scalar("SELECT source FROM tapes WHERE music_api_id = 'tape-42'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(upload, "upload");
    let ready: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tapes WHERE status = 'ready' AND visible = 0")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ready, 2);
    let duration: i64 = sqlx::query_scalar("SELECT duration_ms FROM tapes WHERE source = 'live'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(duration, 2 * 60 * 60 * 1000);
    let format: String =
        sqlx::query_scalar("SELECT format FROM tapes WHERE music_api_id = 'tape-42'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(format, "mp3");
}

#[tokio::test]
async fn apply_refuses_to_overwrite_existing_backup() {
    let (_dir, path, pool) = setup().await;
    let bak = PathBuf::from(format!("{}.bak", path.display()));
    std::fs::write(&bak, b"sentinel").unwrap();

    let err = run(Config {
        legacy: fixture("legacy_export.json"),
        db: path,
        apply: true,
    })
    .await
    .unwrap_err();

    assert_eq!(
        err.exit_code(),
        2,
        "existing backup is an I/O error: {err:?}"
    );
    // Nothing imported.
    assert_eq!(count(&pool, "casters").await, 0);
    assert_eq!(count(&pool, "tapes").await, 0);
    // Sentinel untouched.
    assert_eq!(std::fs::read(&bak).unwrap(), b"sentinel");
}
