//! Schema invariant coverage for `0001_init.sql` (Issue #7).
//!
//! Encodes the invariants from `docs/consolidated-architecture.md` §3.2,
//! §3.3, §3.4 and the permission matrix §6.10.1. Every CHECK/UNIQUE/FK
//! constraint documented there gets at least one positive and one negative
//! case.

use sqlx::Row;
use sqlx::SqlitePool;
use tempfile::TempDir;

const TS: &str = "2026-11-18T12:00:00.000Z";
const T1: &str = "2026-11-18T12:00:00.000Z";
const T2: &str = "2026-11-18T13:00:00.000Z";

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";

async fn setup() -> (TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("connect + migrate");
    (dir, pool)
}

/// Insert a caster with the given id; username is derived from the id.
async fn caster(pool: &SqlitePool, id: &str) -> String {
    let username = format!("user_{}", &id[..8]);
    sqlx::query(
        "INSERT INTO casters (id, username, display_name, password_hash, role_id, created_at, updated_at) \
         VALUES (?, ?, 'Test Caster', 'hash', ?, ?, ?)",
    )
    .bind(id)
    .bind(username)
    .bind(ROLE_CASTER)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert caster");
    id.to_string()
}

async fn show(pool: &SqlitePool, id: &str, slug: &str, created_by: &str) {
    sqlx::query(
        "INSERT INTO shows (id, slug, name, created_by, created_at, updated_at) \
         VALUES (?, ?, 'Test Show', ?, ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(created_by)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert show");
}

async fn queue(pool: &SqlitePool, id: &str, caster_id: &str) {
    queue_named(pool, id, caster_id, "main").await;
}

async fn queue_named(pool: &SqlitePool, id: &str, caster_id: &str, name: &str) {
    sqlx::query(
        "INSERT INTO queues (id, caster_id, name, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(name)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert queue");
}

async fn track(pool: &SqlitePool, id: &str, music_api_id: &str) {
    sqlx::query(
        "INSERT INTO tracks (id, music_api_id, title, duration_ms, storage_key, created_at, updated_at) \
         VALUES (?, ?, 'Track', 1000, 's3/key', ?, ?)",
    )
    .bind(id)
    .bind(music_api_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert track");
}

async fn tape(pool: &SqlitePool, id: &str, slug: &str, caster_id: &str) {
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 'live', ?, ?)",
    )
    .bind(id)
    .bind(slug)
    .bind(caster_id)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .expect("insert tape");
}

#[allow(clippy::too_many_arguments)]
async fn broadcast(
    pool: &SqlitePool,
    id: &str,
    caster_id: &str,
    kind: &str,
    starts_at: &str,
    ends_at: Option<&str>,
    tape_id: Option<&str>,
    queue_id: Option<&str>,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO broadcasts \
         (id, caster_id, kind, title, starts_at, ends_at, tape_id, queue_id, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, 'Slot', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(caster_id)
    .bind(kind)
    .bind(starts_at)
    .bind(ends_at)
    .bind(tape_id)
    .bind(queue_id)
    .bind(caster_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
}

#[allow(clippy::too_many_arguments)]
async fn queue_item(
    pool: &SqlitePool,
    id: &str,
    queue_id: &str,
    position: i64,
    kind: &str,
    track_id: Option<&str>,
    tape_id: Option<&str>,
    link_url: Option<&str>,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO queue_items \
         (id, queue_id, position, kind, track_id, tape_id, link_url, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(queue_id)
    .bind(position)
    .bind(kind)
    .bind(track_id)
    .bind(tape_id)
    .bind(link_url)
    .bind(TS)
    .execute(pool)
    .await
}

// ---------------------------------------------------------------------------
// 1. Migration on a fresh DB + idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migration_creates_18_tables_and_is_clean() {
    let (_dir, pool) = setup().await;

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .expect("count tables");
    assert_eq!(count, 18, "expected exactly 18 user tables");

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
async fn reapplying_migration_is_a_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("zukka.db");

    {
        let pool = zukka_scheduler::db::connect(&path)
            .await
            .expect("first migrate");
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('marker', 'before', ?)")
            .bind(TS)
            .execute(&pool)
            .await
            .expect("insert marker");
    }

    // Second `connect` runs `sqlx::migrate!` again against the same file.
    let pool = zukka_scheduler::db::connect(&path)
        .await
        .expect("second connect must be a no-op");

    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("count migrations");
    assert_eq!(
        applied, 3,
        "migrations `0001` + `0002` + `0003` are tracked exactly once"
    );

    let marker: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'marker'")
        .fetch_one(&pool)
        .await
        .expect("marker survives second migrate");
    assert_eq!(marker, "before");

    let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles")
        .fetch_one(&pool)
        .await
        .expect("count roles");
    assert_eq!(roles, 3, "seed must not duplicate on re-run");
}

// ---------------------------------------------------------------------------
// 2. UUIDv7: app responsibility, DB guarantees TEXT storage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn primary_keys_are_text_not_blob_or_integer() {
    let (_dir, pool) = setup().await;

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
        "tape_jobs",
        "upload_jobs",
        "announcement_templates",
        "announcements",
        "event_log",
    ];
    for table in expected {
        let rows = sqlx::query(&format!("PRAGMA table_info('{table}')"))
            .fetch_all(&pool)
            .await
            .expect("table_info");
        let id_type: String = rows
            .iter()
            .find(|r| r.get::<String, _>("name") == "id")
            .map(|r| r.get::<String, _>("type"))
            .unwrap_or_else(|| panic!("table {table} must have an id column"));
        assert_eq!(
            id_type.to_uppercase(),
            "TEXT",
            "table {table}.id must be TEXT (UUIDv7 app-generated)"
        );
    }

    // `settings` uses `key` as its TEXT primary key instead of `id`.
    let rows = sqlx::query("PRAGMA table_info('settings')")
        .fetch_all(&pool)
        .await
        .expect("settings table_info");
    let key_type: String = rows
        .iter()
        .find(|r| r.get::<String, _>("name") == "key")
        .map(|r| r.get::<String, _>("type"))
        .expect("settings.key column");
    assert_eq!(key_type.to_uppercase(), "TEXT");

    // No CHECK can validate UUIDv7, so the app supplies the value verbatim.
    let id = "0192f0a1-0001-7000-8000-00000000abcd";
    caster(&pool, id).await;
    let (stored, ty): (String, String) =
        sqlx::query_as("SELECT id, typeof(id) FROM casters WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("read id");
    assert_eq!(stored, id);
    assert_eq!(ty, "text");

    // Arbitrary text is accepted: the DB intentionally does not enforce UUIDv7.
    // This documents that validation lives in the application layer.
    let arbitrary =
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('not-a-uuid', 'x', ?)")
            .bind(TS)
            .execute(&pool)
            .await;
    assert!(arbitrary.is_ok(), "TEXT PKs are not format-checked in DB");
}

// ---------------------------------------------------------------------------
// 3. broadcasts: open end, ordering, kind/content matrix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn broadcasts_open_end_allowed() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;

    broadcast(
        &pool,
        "b0000000-0000-7000-8000-000000000001",
        &c,
        "live",
        T1,
        None,
        None,
        None,
    )
    .await
    .expect("live slot with open end is valid");
}

#[tokio::test]
async fn broadcasts_rejects_non_increasing_end() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;

    let earlier = broadcast(
        &pool,
        "b0000000-0000-7000-8000-000000000002",
        &c,
        "live",
        T2,
        Some(T1),
        None,
        None,
    )
    .await;
    assert!(earlier.is_err(), "ends_at < starts_at must be rejected");

    let equal = broadcast(
        &pool,
        "b0000000-0000-7000-8000-000000000003",
        &c,
        "live",
        T1,
        Some(T1),
        None,
        None,
    )
    .await;
    assert!(equal.is_err(), "ends_at == starts_at must be rejected");
}

#[tokio::test]
async fn broadcasts_content_matrix() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    show(&pool, "50000000-0000-7000-8000-000000000001", "morning", &c).await;
    queue(&pool, "a0000000-0000-7000-8000-000000000001", &c).await;
    tape(
        &pool,
        "70000000-0000-7000-8000-000000000001",
        "TAPE0001",
        &c,
    )
    .await;

    // live: no content
    broadcast(
        &pool,
        "b1000000-0000-7000-8000-000000000001",
        &c,
        "live",
        T1,
        Some(T2),
        None,
        None,
    )
    .await
    .expect("live without content is valid");
    broadcast(
        &pool,
        "b1000000-0000-7000-8000-000000000002",
        &c,
        "live",
        T1,
        Some(T2),
        None,
        Some("a0000000-0000-7000-8000-000000000001"),
    )
    .await
    .expect_err("live with queue must be rejected");

    // recording: exactly tape
    broadcast(
        &pool,
        "b1000000-0000-7000-8000-000000000003",
        &c,
        "recording",
        T1,
        Some(T2),
        Some("70000000-0000-7000-8000-000000000001"),
        None,
    )
    .await
    .expect("recording with tape is valid");
    broadcast(
        &pool,
        "b1000000-0000-7000-8000-000000000004",
        &c,
        "recording",
        T1,
        Some(T2),
        None,
        None,
    )
    .await
    .expect_err("recording without tape must be rejected");

    // playlist: exactly queue
    broadcast(
        &pool,
        "b1000000-0000-7000-8000-000000000005",
        &c,
        "playlist",
        T1,
        Some(T2),
        None,
        Some("a0000000-0000-7000-8000-000000000001"),
    )
    .await
    .expect("playlist with queue is valid");
    broadcast(
        &pool,
        "b1000000-0000-7000-8000-000000000006",
        &c,
        "playlist",
        T1,
        Some(T2),
        Some("70000000-0000-7000-8000-000000000001"),
        None,
    )
    .await
    .expect_err("playlist with tape must be rejected");
}

// ---------------------------------------------------------------------------
// 4. queue_items: exactly one content field + UNIQUE(queue_id, position)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_items_exactly_one_content_field() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    queue(&pool, "a0000000-0000-7000-8000-000000000001", &c).await;
    track(&pool, "d0000000-0000-7000-8000-000000000001", "m1").await;
    tape(
        &pool,
        "70000000-0000-7000-8000-000000000001",
        "TAPE0001",
        &c,
    )
    .await;

    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000001",
        "a0000000-0000-7000-8000-000000000001",
        0,
        "track",
        Some("d0000000-0000-7000-8000-000000000001"),
        None,
        None,
    )
    .await
    .expect("track item valid");
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000002",
        "a0000000-0000-7000-8000-000000000001",
        1,
        "tape",
        None,
        Some("70000000-0000-7000-8000-000000000001"),
        None,
    )
    .await
    .expect("tape item valid");
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000003",
        "a0000000-0000-7000-8000-000000000001",
        2,
        "link",
        None,
        None,
        Some("https://example.com/x"),
    )
    .await
    .expect("link item valid");

    // two content fields
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000004",
        "a0000000-0000-7000-8000-000000000001",
        3,
        "track",
        Some("d0000000-0000-7000-8000-000000000001"),
        Some("70000000-0000-7000-8000-000000000001"),
        None,
    )
    .await
    .expect_err("track + tape must be rejected");

    // no content field
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000005",
        "a0000000-0000-7000-8000-000000000001",
        4,
        "link",
        None,
        None,
        None,
    )
    .await
    .expect_err("link without link_url must be rejected");

    // kind mismatch
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000006",
        "a0000000-0000-7000-8000-000000000001",
        5,
        "tape",
        Some("d0000000-0000-7000-8000-000000000001"),
        None,
        None,
    )
    .await
    .expect_err("kind=tape with only track_id must be rejected");
}

#[tokio::test]
async fn queue_items_position_is_unique_per_queue() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    queue(&pool, "a0000000-0000-7000-8000-000000000001", &c).await;

    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000001",
        "a0000000-0000-7000-8000-000000000001",
        0,
        "link",
        None,
        None,
        Some("https://example.com/a"),
    )
    .await
    .expect("first item");
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000002",
        "a0000000-0000-7000-8000-000000000001",
        0,
        "link",
        None,
        None,
        Some("https://example.com/b"),
    )
    .await
    .expect_err("duplicate (queue_id, position) must be rejected");

    // position is scoped per queue
    queue_named(&pool, "a0000000-0000-7000-8000-000000000002", &c, "second").await;
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000003",
        "a0000000-0000-7000-8000-000000000002",
        0,
        "link",
        None,
        None,
        Some("https://example.com/c"),
    )
    .await
    .expect("same position in another queue is allowed");
}

// ---------------------------------------------------------------------------
// 5. tapes: enum CHECKs, timestamp ordering, slug UNIQUE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tapes_status_source_visible_checks() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;

    // invalid status
    let err = sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, status, created_at, updated_at) \
         VALUES ('70000000-0000-7000-8000-00000000000a', 'BADSTAT1', ?, ?, ?, 'live', 'bogus', ?, ?)",
    )
    .bind(&c)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(err.is_err(), "invalid tapes.status must be rejected");

    // invalid source
    let err = sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES ('70000000-0000-7000-8000-00000000000b', 'BADSRC01', ?, ?, ?, 'relay', ?, ?)",
    )
    .bind(&c)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(err.is_err(), "invalid tapes.source must be rejected");

    // invalid visible
    let err = sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, visible, created_at, updated_at) \
         VALUES ('70000000-0000-7000-8000-00000000000c', 'BADVIS01', ?, ?, ?, 'live', 2, ?, ?)",
    )
    .bind(&c)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(err.is_err(), "invalid tapes.visible must be rejected");

    // positive baseline for all three enums
    sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, status, visible, created_at, updated_at) \
         VALUES ('70000000-0000-7000-8000-00000000000d', 'GOOD0001', ?, ?, ?, 'upload', 'ready', 1, ?, ?)",
    )
    .bind(&c)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("valid tape row");
}

#[tokio::test]
async fn tapes_reject_inverted_timestamps_and_duplicate_slug() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;

    let inverted = sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES ('70000000-0000-7000-8000-000000000010', 'INVERT01', ?, ?, ?, 'live', ?, ?)",
    )
    .bind(&c)
    .bind(T2)
    .bind(T1)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(inverted.is_err(), "ended_at < started_at must be rejected");

    tape(
        &pool,
        "70000000-0000-7000-8000-000000000011",
        "DUP00001",
        &c,
    )
    .await;
    let dup = sqlx::query(
        "INSERT INTO tapes (id, slug, caster_id, started_at, ended_at, source, created_at, updated_at) \
         VALUES ('70000000-0000-7000-8000-000000000012', 'DUP00001', ?, ?, ?, 'live', ?, ?)",
    )
    .bind(&c)
    .bind(T1)
    .bind(T2)
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(dup.is_err(), "duplicate tapes.slug must be rejected");
}

// ---------------------------------------------------------------------------
// 6. sources: priority bounds + fixed seed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sources_priority_bounds() {
    let (_dir, pool) = setup().await;

    // The six layer names are fixed and seeded, so bounds are probed via
    // UPDATE on an existing row (priority is app-immutable, CHECK still bites).
    for bad in [0, 101, -5] {
        let res = sqlx::query("UPDATE sources SET priority = ? WHERE name = 'live'")
            .bind(bad)
            .execute(&pool)
            .await;
        assert!(res.is_err(), "priority {bad} must be rejected");
    }

    let live_priority: i64 = sqlx::query_scalar("SELECT priority FROM sources WHERE name = 'live'")
        .fetch_one(&pool)
        .await
        .expect("read live priority");
    assert_eq!(live_priority, 100, "priority 100 remains valid");
}

#[tokio::test]
async fn sources_seed_matches_layers() {
    let (_dir, pool) = setup().await;

    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT name, priority FROM sources ORDER BY priority DESC")
            .fetch_all(&pool)
            .await
            .expect("read sources");
    let expected = vec![
        ("live".to_string(), 100),
        ("show".to_string(), 80),
        ("queue".to_string(), 60),
        ("rotation".to_string(), 40),
        ("relay".to_string(), 20),
        ("fun".to_string(), 1),
    ];
    assert_eq!(rows, expected, "exactly the 6 layers with fixed priorities");
}

// ---------------------------------------------------------------------------
// 7. roles seed + permission matrix (§6.10.1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn roles_permission_matrix() {
    let (_dir, pool) = setup().await;

    let names: Vec<(String,)> = sqlx::query_as("SELECT name FROM roles ORDER BY name")
        .fetch_all(&pool)
        .await
        .expect("roles");
    assert_eq!(
        names.into_iter().map(|r| r.0).collect::<Vec<_>>(),
        vec!["admin", "caster", "operator"]
    );

    // (permission, caster, operator, admin)
    let matrix = [
        ("library.read", true, true, true),
        ("library.upload", true, true, true),
        ("library.delete", false, true, true),
        ("queue.write.own", true, true, true),
        ("queue.write.any", false, true, true),
        ("show.write", true, true, true),
        ("broadcast.write", true, true, true),
        ("control.read", true, true, true),
        ("control.write", false, true, true),
        ("announcement.write", true, true, true),
        ("tape.publish", true, true, true),
        ("caster.manage", false, true, true),
        ("settings.manage", false, false, true),
        ("event_log.read", false, true, true),
    ];

    for (perm, want_caster, want_operator, want_admin) in matrix {
        for (role, want) in [
            ("caster", want_caster),
            ("operator", want_operator),
            ("admin", want_admin),
        ] {
            let got: Option<i64> =
                sqlx::query_scalar("SELECT json_extract(permissions, ?) FROM roles WHERE name = ?")
                    .bind(format!("$.\"{perm}\""))
                    .bind(role)
                    .fetch_one(&pool)
                    .await
                    .expect("read permission");
            assert_eq!(
                got,
                Some(i64::from(want)),
                "role {role} permission {perm} must be {want}"
            );
        }
    }

    // The matrix is complete: every documented permission exists, nothing extra.
    for role in ["admin", "operator", "caster"] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM json_each((SELECT permissions FROM roles WHERE name = ?))",
        )
        .bind(role)
        .fetch_one(&pool)
        .await
        .expect("permission count");
        assert_eq!(count, 14, "role {role} must have exactly 14 permissions");
    }
}

// ---------------------------------------------------------------------------
// 8. sessions: idle_expires_at < absolute_expires_at
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sessions_expiry_ordering() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;

    sqlx::query(
        "INSERT INTO sessions \
         (id, caster_id, token_hash, csrf_secret, created_at, last_seen_at, idle_expires_at, absolute_expires_at) \
         VALUES ('51000000-0000-7000-8000-000000000001', ?, 'hash1', 'csrf', ?, ?, ?, ?)",
    )
    .bind(&c)
    .bind(TS)
    .bind(TS)
    .bind(T1)
    .bind(T2)
    .execute(&pool)
    .await
    .expect("idle < absolute is valid");

    let equal = sqlx::query(
        "INSERT INTO sessions \
         (id, caster_id, token_hash, csrf_secret, created_at, last_seen_at, idle_expires_at, absolute_expires_at) \
         VALUES ('51000000-0000-7000-8000-000000000002', ?, 'hash2', 'csrf', ?, ?, ?, ?)",
    )
    .bind(&c)
    .bind(TS)
    .bind(TS)
    .bind(T1)
    .bind(T1)
    .execute(&pool)
    .await;
    assert!(equal.is_err(), "idle == absolute must be rejected");

    let inverted = sqlx::query(
        "INSERT INTO sessions \
         (id, caster_id, token_hash, csrf_secret, created_at, last_seen_at, idle_expires_at, absolute_expires_at) \
         VALUES ('51000000-0000-7000-8000-000000000003', ?, 'hash3', 'csrf', ?, ?, ?, ?)",
    )
    .bind(&c)
    .bind(TS)
    .bind(TS)
    .bind(T2)
    .bind(T1)
    .execute(&pool)
    .await;
    assert!(inverted.is_err(), "idle > absolute must be rejected");
}

// ---------------------------------------------------------------------------
// 9. announcement_templates: partial UNIQUE (global vs per show)
// ---------------------------------------------------------------------------

async fn template(
    pool: &SqlitePool,
    id: &str,
    event: &str,
    show_id: Option<&str>,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO announcement_templates (id, name, event, show_id, body, created_at, updated_at) \
         VALUES (?, 'tpl', ?, ?, 'body', ?, ?)",
    )
    .bind(id)
    .bind(event)
    .bind(show_id)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
}

#[tokio::test]
async fn announcement_templates_partial_unique() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    show(&pool, "50000000-0000-7000-8000-000000000001", "one", &c).await;
    show(&pool, "50000000-0000-7000-8000-000000000002", "two", &c).await;

    template(
        &pool,
        "60000000-0000-7000-8000-000000000001",
        "live_on",
        None,
    )
    .await
    .expect("global template");
    template(
        &pool,
        "60000000-0000-7000-8000-000000000002",
        "live_on",
        None,
    )
    .await
    .expect_err("second global template for same event/channel must be rejected");

    template(
        &pool,
        "60000000-0000-7000-8000-000000000003",
        "live_on",
        Some("50000000-0000-7000-8000-000000000001"),
    )
    .await
    .expect("per-show template alongside global is allowed");
    template(
        &pool,
        "60000000-0000-7000-8000-000000000004",
        "live_on",
        Some("50000000-0000-7000-8000-000000000001"),
    )
    .await
    .expect_err("duplicate per-show template must be rejected");
    template(
        &pool,
        "60000000-0000-7000-8000-000000000005",
        "live_on",
        Some("50000000-0000-7000-8000-000000000002"),
    )
    .await
    .expect("same event for a different show is allowed");
}

// ---------------------------------------------------------------------------
// 10. announcements.dedupe_key: UNIQUE, multiple NULLs allowed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn announcements_dedupe_key_unique_nullable() {
    let (_dir, pool) = setup().await;

    sqlx::query(
        "INSERT INTO announcements (id, event, channel, dedupe_key, created_at, updated_at) \
         VALUES ('61000000-0000-7000-8000-000000000001', 'fallback_entered', 'telegram', 'k1', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await
    .expect("first dedupe key");

    let dup = sqlx::query(
        "INSERT INTO announcements (id, event, channel, dedupe_key, created_at, updated_at) \
         VALUES ('61000000-0000-7000-8000-000000000002', 'fallback_entered', 'telegram', 'k1', ?, ?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(&pool)
    .await;
    assert!(dup.is_err(), "duplicate dedupe_key must be rejected");

    for (i, id) in [
        "61000000-0000-7000-8000-000000000003",
        "61000000-0000-7000-8000-000000000004",
    ]
    .iter()
    .enumerate()
    {
        sqlx::query(
            "INSERT INTO announcements (id, event, channel, dedupe_key, created_at, updated_at) \
             VALUES (?, 'manual', 'telegram', NULL, ?, ?)",
        )
        .bind(*id)
        .bind(TS)
        .bind(TS)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("NULL dedupe_key #{i} must be allowed: {e}"));
    }

    let nulls: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM announcements WHERE dedupe_key IS NULL")
            .fetch_one(&pool)
            .await
            .expect("count null dedupe keys");
    assert_eq!(nulls, 2);
}

// ---------------------------------------------------------------------------
// 12. FK behaviour: RESTRICT and CASCADE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_role_with_casters_is_restricted() {
    let (_dir, pool) = setup().await;
    caster(&pool, "c0000000-0000-7000-8000-000000000001").await;

    let res = sqlx::query("DELETE FROM roles WHERE id = ?")
        .bind(ROLE_CASTER)
        .execute(&pool)
        .await;
    assert!(
        res.is_err(),
        "deleting a referenced role must be restricted"
    );
}

#[tokio::test]
async fn delete_queue_cascades_queue_items() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    queue(&pool, "a0000000-0000-7000-8000-000000000001", &c).await;
    queue_item(
        &pool,
        "b0000000-0000-7000-8000-000000000001",
        "a0000000-0000-7000-8000-000000000001",
        0,
        "link",
        None,
        None,
        Some("https://example.com/x"),
    )
    .await
    .expect("queue item");

    sqlx::query("DELETE FROM queues WHERE id = 'a0000000-0000-7000-8000-000000000001'")
        .execute(&pool)
        .await
        .expect("delete queue");

    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_items")
        .fetch_one(&pool)
        .await
        .expect("count items");
    assert_eq!(items, 0, "queue_items must cascade with their queue");
}

#[tokio::test]
async fn delete_caster_cascades_queues() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    queue(&pool, "a0000000-0000-7000-8000-000000000001", &c).await;

    sqlx::query("DELETE FROM casters WHERE id = ?")
        .bind(&c)
        .execute(&pool)
        .await
        .expect("delete caster");

    let queues: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queues")
        .fetch_one(&pool)
        .await
        .expect("count queues");
    assert_eq!(queues, 0, "queues must cascade with their caster");
}

// ---------------------------------------------------------------------------
// 13. show_announcement_timings: STRICT, CHECK bounds, FK cascade (Issue #36)
// ---------------------------------------------------------------------------

async fn timing_override(
    pool: &SqlitePool,
    show_id: &str,
    lead: Option<i64>,
    live_on: Option<i64>,
    live_off: Option<i64>,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO show_announcement_timings \
         (show_id, show_reminder_lead_seconds, live_on_delay_seconds, live_off_delay_seconds, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(show_id)
    .bind(lead)
    .bind(live_on)
    .bind(live_off)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
}

#[tokio::test]
async fn show_announcement_timings_is_strict_and_bounded() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    show(&pool, "50000000-0000-7000-8000-000000000001", "one", &c).await;
    let sid = "50000000-0000-7000-8000-000000000001";

    // The table is STRICT.
    let strict: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'show_announcement_timings'",
    )
    .fetch_one(&pool)
    .await
    .expect("read table sql");
    assert!(
        strict.contains("STRICT"),
        "show_announcement_timings must be STRICT"
    );

    // One override is enough.
    timing_override(&pool, sid, Some(300), None, None)
        .await
        .expect("single override is valid");
    sqlx::query("DELETE FROM show_announcement_timings WHERE show_id = ?")
        .bind(sid)
        .execute(&pool)
        .await
        .expect("cleanup");

    // All-NULL row is rejected by the table CHECK.
    timing_override(&pool, sid, None, None, None)
        .await
        .expect_err("an all-NULL override row must be rejected");

    // Negative and out-of-range values are rejected.
    timing_override(&pool, sid, Some(-1), None, None)
        .await
        .expect_err("negative lead must be rejected");
    timing_override(&pool, sid, Some(604_801), None, None)
        .await
        .expect_err("lead > 604800 must be rejected");

    // Boundaries are accepted.
    timing_override(&pool, sid, Some(0), Some(604_800), None)
        .await
        .expect("0 and 604800 are valid");
}

#[tokio::test]
async fn show_announcement_timings_cascade_with_show() {
    let (_dir, pool) = setup().await;
    let c = caster(&pool, "c0000000-0000-7000-8000-000000000001").await;
    show(&pool, "50000000-0000-7000-8000-000000000002", "two", &c).await;
    let sid = "50000000-0000-7000-8000-000000000002";
    timing_override(&pool, sid, Some(300), None, None)
        .await
        .expect("override");

    sqlx::query("DELETE FROM shows WHERE id = ?")
        .bind(sid)
        .execute(&pool)
        .await
        .expect("delete show");

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM show_announcement_timings")
        .fetch_one(&pool)
        .await
        .expect("count overrides");
    assert_eq!(rows, 0, "overrides must cascade with their show");
}
