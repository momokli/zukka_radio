//! Cross-crate end-to-end tests: the portal's [`HttpQueueApi`] and
//! [`HttpControlApi`] against the **real** scheduler (Issue #20).
//!
//! `zukka_scheduler::api::router` is served on an ephemeral TCP port over a
//! migrated temporary SQLite database, and the portal's production clients talk
//! to it. This exercises the true portal↔scheduler contract: status mapping
//! (`404`/`409`/`422`) and the reorder optimistic-lock proof.

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_portal::api_client::{
    ControlApi, ControlError, HttpControlApi, HttpQueueApi, NewQueue, NewQueueItem, QueueApi,
    QueueError, QueueQuery,
};

use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-19T12:00:00.000Z";

struct Scheduler {
    _dir: TempDir,
    base_url: String,
    pool: SqlitePool,
}

impl Scheduler {
    async fn boot() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = zukka_scheduler::db::connect(dir.path().join("zukka.db"))
            .await
            .expect("connect + migrate");
        let app = zukka_scheduler::api::router(pool.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scheduler");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            _dir: dir,
            base_url: format!("http://{addr}"),
            pool,
        }
    }

    async fn seed_caster(&self) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
             created_at, updated_at) VALUES (?, ?, 'X', 'h', ?, 1, ?, ?)",
        )
        .bind(&id)
        .bind(format!("u_{}", id.replace('-', "")))
        .bind(ROLE_CASTER)
        .bind(TS)
        .bind(TS)
        .execute(&self.pool)
        .await
        .expect("insert caster");
        id
    }

    async fn seed_track(&self, caster: &str) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO tracks (id, music_api_id, title, duration_ms, storage_key, visibility, \
             caster_id, created_at, updated_at) VALUES (?, ?, 'Song', 1000, 'key', 'shared', ?, ?, ?)",
        )
        .bind(&id)
        .bind(format!("m_{}", id.replace('-', "")))
        .bind(caster)
        .bind(TS)
        .bind(TS)
        .execute(&self.pool)
        .await
        .expect("insert track");
        id
    }

    fn queues(&self) -> HttpQueueApi {
        HttpQueueApi::new(self.base_url.clone(), "svc-token").expect("queue client")
    }

    fn control(&self) -> HttpControlApi {
        HttpControlApi::new(self.base_url.clone(), "svc-token").expect("control client")
    }
}

/// Full CRUD round trip through the real scheduler.
#[tokio::test]
async fn http_queue_api_crud_roundtrip() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let track = s.seed_track(&caster).await;
    let api = s.queues();

    let created = api
        .create(
            Some(&caster),
            &NewQueue {
                caster_id: caster.clone(),
                name: "Set 1".into(),
                description: Some("first".into()),
                kind: None,
            },
        )
        .await
        .expect("create");
    assert_eq!(created.version, 1);
    assert_eq!(created.kind, "background");

    let listed = api
        .list(
            Some(&caster),
            &QueueQuery {
                caster_id: Some(caster.clone()),
                kind: None,
            },
        )
        .await
        .expect("list");
    assert!(listed.iter().any(|q| q.id == created.id));

    let track_item = api
        .add_item(
            Some(&caster),
            &created.id,
            &NewQueueItem {
                kind: "track".into(),
                track_id: Some(track),
                ..Default::default()
            },
        )
        .await
        .expect("add track");
    assert_eq!(track_item.position, 0);

    let link_item = api
        .add_item(
            Some(&caster),
            &created.id,
            &NewQueueItem {
                kind: "link".into(),
                link_url: Some("https://youtu.be/abc".into()),
                link_provider: Some("youtube".into()),
                ..Default::default()
            },
        )
        .await
        .expect("add link");
    assert_eq!(link_item.position, 1);
    assert_eq!(link_item.link_state.as_deref(), Some("pending"));

    // Reorder (reversed) with the current version.
    let current = api.get(Some(&caster), &created.id).await.expect("get");
    let reordered = api
        .reorder(
            Some(&caster),
            &created.id,
            &[link_item.id.clone(), track_item.id.clone()],
            current.version,
        )
        .await
        .expect("reorder");
    assert_eq!(reordered.version, current.version + 1);

    let items = api.items(Some(&caster), &created.id).await.expect("items");
    assert_eq!(items[0].id, link_item.id, "reorder persisted");
    assert_eq!(items[1].id, track_item.id);

    api.delete(Some(&caster), &created.id, reordered.version)
        .await
        .expect("delete");
    assert!(
        api.get(Some(&caster), &created.id)
            .await
            .unwrap_err()
            .is_not_found()
    );
}

/// Proof: a reorder with a stale version is a `409` (`Conflict`) and does not
/// mutate the queue.
#[tokio::test]
async fn http_queue_api_reorder_is_version_guarded() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let track = s.seed_track(&caster).await;
    let api = s.queues();

    let q = api
        .create(
            Some(&caster),
            &NewQueue {
                caster_id: caster.clone(),
                name: "Set 2".into(),
                ..Default::default()
            },
        )
        .await
        .expect("create");
    let i1 = api
        .add_item(
            Some(&caster),
            &q.id,
            &NewQueueItem {
                kind: "track".into(),
                track_id: Some(track.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("i1");
    let i2 = api
        .add_item(
            Some(&caster),
            &q.id,
            &NewQueueItem {
                kind: "track".into(),
                track_id: Some(track),
                ..Default::default()
            },
        )
        .await
        .expect("i2");

    let err = api
        .reorder(
            Some(&caster),
            &q.id,
            &[i2.id.clone(), i1.id.clone()],
            q.version + 100,
        )
        .await
        .expect_err("stale version");
    assert!(err.is_conflict(), "409 maps to Conflict: {err}");

    let items = api.items(Some(&caster), &q.id).await.expect("items");
    assert_eq!(items[0].id, i1.id, "unchanged on conflict");
}

/// Status mapping of the real endpoints: `422` blank name / wrong item set,
/// `404` unknown queue.
#[tokio::test]
async fn http_queue_api_maps_422_and_404() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;
    let api = s.queues();

    // 422: blank name.
    let err = api
        .create(
            Some(&caster),
            &NewQueue {
                caster_id: caster.clone(),
                name: "   ".into(),
                ..Default::default()
            },
        )
        .await
        .expect_err("blank name");
    assert!(
        matches!(err, QueueError::Validation { ref field, .. } if field == "name"),
        "422 maps to Validation{{name}}: {err}"
    );

    // 422: reorder with a wrong item set.
    let q = api
        .create(
            Some(&caster),
            &NewQueue {
                caster_id: caster.clone(),
                name: "Set 3".into(),
                ..Default::default()
            },
        )
        .await
        .expect("create");
    let err = api
        .reorder(Some(&caster), &q.id, &[new_id()], q.version)
        .await
        .expect_err("wrong set");
    assert!(
        matches!(err, QueueError::Validation { ref field, .. } if field == "item_ids"),
        "422 maps to Validation{{item_ids}}: {err}"
    );

    // 404: unknown queue.
    let err = api
        .get(Some(&caster), &new_id())
        .await
        .expect_err("unknown");
    assert!(err.is_not_found(), "404 maps to NotFound: {err}");
}

/// `HttpControlApi::skip` against the real scheduler: `204` on success, `422`
/// (empty reason) maps to a `Conflict`.
#[tokio::test]
async fn http_control_api_skip_maps_errors() {
    let s = Scheduler::boot().await;
    let control = s.control();

    control.skip("manual").await.expect("skip ok");

    let err = control.skip("   ").await.expect_err("empty reason");
    assert!(matches!(err, ControlError::Conflict(_)), "{err}");
}
