//! Smoke test for migration `0001_init.sql` (Issue #7).
//!
//! Verifies: table count, FK integrity, integrity_check, a happy-path insert
//! chain, FK rejection, and the `event_log` append-only trigger.

use sqlx::Row;
use sqlx::sqlite::SqlitePool;
use tempfile::TempDir;

const TS: &str = "2026-11-18T12:00:00.000Z";
const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

#[tokio::test]
async fn migration_creates_17_user_tables() {
    let (_dir, pool) = setup().await;

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' \
         AND name != '_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .expect("count tables");
    assert_eq!(count, 17, "expected exactly 17 user tables");

    let expected = [
        "roles",
        "casters",
        "sessions",
        "shows",
        "broadcasts",
        "sources",
        "queues",
        "queue_items",
        "tracks",
        "tapes",
        "waveforms",
        "tape_jobs",
        "upload_jobs",
        "announcement_templates",
        "announcements",
        "event_log",
        "settings",
    ];
    for name in expected {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("lookup table");
        assert_eq!(exists, 1, "table {name} must exist");
    }
}

#[tokio::test]
async fn foreign_key_check_is_empty_and_integrity_ok() {
    let (_dir, pool) = setup().await;

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
}

#[tokio::test]
async fn seed_roles_and_sources_present() {
    let (_dir, pool) = setup().await;

    let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles")
        .fetch_one(&pool)
        .await
        .expect("count roles");
    assert_eq!(roles, 3);

    let sources: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sources")
        .fetch_one(&pool)
        .await
        .expect("count sources");
    assert_eq!(sources, 6);
}

#[tokio::test]
async fn smoke_insert_chain() {
    let (_dir, pool) = setup().await;

    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES ('c0000000-0000-7000-8000-000000000001', 'tester', 'Tester', 'hash', ?, ?, ?)",
    )
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert caster");

    sqlx::query(
        "INSERT INTO shows (id, slug, name, created_by, created_at, updated_at) \
         VALUES ('50000000-0000-7000-8000-000000000001', 'morning', 'Morning Show', \
                 'c0000000-0000-7000-8000-000000000001', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert show");

    sqlx::query(
        "INSERT INTO queues (id, caster_id, name, created_at, updated_at) \
         VALUES ('a0000000-0000-7000-8000-000000000001', \
                 'c0000000-0000-7000-8000-000000000001', 'main', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert queue");

    sqlx::query(
        "INSERT INTO queue_items (id, queue_id, position, kind, link_url, created_at) \
         VALUES ('b0000000-0000-7000-8000-000000000001', \
                 'a0000000-0000-7000-8000-000000000001', 0, 'link', \
                 'https://example.com/x', ?)",
    )
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert queue_item");

    // FK violation: caster referencing a non-existent role must be rejected.
    let bad = sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES ('c0000000-0000-7000-8000-000000000002', 'ghost', 'Ghost', 'hash', \
                 'ffffffff-ffff-7fff-8fff-ffffffffffff', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(bad.is_err(), "FK violation must be rejected");
}

#[tokio::test]
async fn event_log_is_append_only() {
    let (_dir, pool) = setup().await;

    sqlx::query(
        "INSERT INTO event_log (id, occurred_at, event, created_at) \
         VALUES ('e0000000-0000-7000-8000-000000000001', ?, 'fallback', ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("insert event_log");

    let updated = sqlx::query("UPDATE event_log SET event = 'error' WHERE id = ?")
        .bind("e0000000-0000-7000-8000-000000000001")
        .execute(&pool)
        .await;
    assert!(updated.is_err(), "UPDATE on event_log must be rejected");

    let deleted = sqlx::query("DELETE FROM event_log WHERE id = ?")
        .bind("e0000000-0000-7000-8000-000000000001")
        .execute(&pool)
        .await;
    assert!(deleted.is_err(), "DELETE on event_log must be rejected");

    // Row must still be present.
    let row = sqlx::query("SELECT event FROM event_log WHERE id = ?")
        .bind("e0000000-0000-7000-8000-000000000001")
        .fetch_one(&pool)
        .await
        .expect("event_log row present");
    assert_eq!(row.get::<String, _>("event"), "fallback");
}
