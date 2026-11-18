//! Integration test for Issue #9: app-generated UUIDv7 IDs against the real
//! schema.
//!
//! Proves the schema convention (§3.1): IDs are canonical 36-char TEXT,
//! parseable UUIDv7, and lexicographic ordering of the stored PK equals the
//! generation (insertion) order.

use sqlx::sqlite::SqlitePool;
use tempfile::TempDir;
use uuid::Uuid;
use uuid::Version;

const N: usize = 200;
const TS: &str = "2026-11-18T12:00:00.000Z";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

#[tokio::test]
async fn generated_ids_are_canonical_uuidv7() {
    for i in 0..N {
        let id = zukka_scheduler::uuid::new_id();
        assert_eq!(id.len(), 36, "id {i} has unexpected length: {id}");

        let parts: Vec<&str> = id.split('-').collect();
        let lens: Vec<usize> = parts.iter().map(|p| p.len()).collect();
        assert_eq!(lens, vec![8, 4, 4, 4, 12], "id {i} grouping: {id}");

        let parsed = Uuid::parse_str(&id).unwrap_or_else(|e| panic!("id {i} unparseable: {e}"));
        assert_eq!(parsed.get_version_num(), 7, "id {i} is not v7: {id}");
        assert_eq!(parsed.get_version(), Some(Version::SortRand));
    }
}

#[tokio::test]
async fn ids_are_strictly_increasing() {
    let mut prev = zukka_scheduler::uuid::new_id();
    for i in 0..N {
        let cur = zukka_scheduler::uuid::new_id();
        assert!(prev < cur, "not increasing at {i}: {prev} !< {cur}");
        prev = cur;
    }
}

#[tokio::test]
async fn db_order_by_pk_matches_insertion_order() {
    let (_dir, pool) = setup().await;

    // Generate first, insert in that order, then read back via the schema.
    let mut generated: Vec<String> = Vec::with_capacity(N);
    for _ in 0..N {
        generated.push(zukka_scheduler::uuid::new_id());
    }
    for (i, id) in generated.iter().enumerate() {
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind(i.to_string())
            .bind(TS)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("insert {i} ({id}) failed: {e}"));
    }

    let stored_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
        .fetch_one(&pool)
        .await
        .expect("count settings");
    assert_eq!(stored_count as usize, N);

    let mut rows = generated.clone();
    rows.sort(); // SQLite TEXT ordering is bytewise; sort must be identity here.
    assert_eq!(rows, generated, "lexicographic order must be generation order");

    let from_db: Vec<String> = sqlx::query_scalar("SELECT key FROM settings ORDER BY key")
        .fetch_all(&pool)
        .await
        .expect("select ordered keys");
    assert_eq!(from_db.len(), N);
    assert_eq!(from_db, generated, "ORDER BY key must match insertion order");

    // Values travelled with their key: key[i] carries the index it was inserted with.
    let pairs: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(&pool)
            .await
            .expect("select ordered key/value");
    for (i, (key, value)) in pairs.iter().enumerate() {
        assert_eq!(key, &generated[i], "key at position {i}");
        assert_eq!(value, &i.to_string(), "value at position {i}");
    }
}