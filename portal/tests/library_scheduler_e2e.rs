//! Cross-crate end-to-end tests: the portal's [`HttpTrackApi`] against the
//! **real** scheduler (Issue #19, US-2/US-3).
//!
//! `zukka_scheduler::api::router` is served on an ephemeral TCP port over a
//! migrated temporary SQLite database, and the portal's production
//! [`HttpTrackApi`] talks to it. This exercises the true portal↔scheduler
//! contract for the library — not a fake: status mapping
//! (`404`/`409`/`422` → [`TrackError`]) and the `X-Caster-Id` visibility scope
//! (§1 core acceptance: a foreign private track is invisible in list/search and
//! via a direct URL, always `404`, never `403`).

use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_portal::api_client::{HttpTrackApi, NewTrackRequest, TrackApi, TrackError, TrackQuery};

use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-18T12:00:00.000Z";

/// A live scheduler on an ephemeral port + the migrated database behind it.
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

    /// Seeds a caster (needed because `tracks.caster_id` is an FK).
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

    fn tracks(&self) -> HttpTrackApi {
        HttpTrackApi::new(self.base_url.clone(), "svc-token").expect("track client")
    }
}

fn new_track(
    music_api_id: &str,
    title: &str,
    visibility: &str,
    caster: Option<&str>,
) -> NewTrackRequest {
    NewTrackRequest {
        music_api_id: music_api_id.to_string(),
        isrc: None,
        title: title.to_string(),
        storage_key: format!("key/{music_api_id}"),
        duration_ms: 1000,
        artist: Some("Artist".into()),
        album: Some("Album".into()),
        artwork_url: None,
        visibility: Some(visibility.to_string()),
        caster_id: caster.map(str::to_string),
    }
}

fn ids(tracks: &[zukka_portal::api_client::Track]) -> Vec<String> {
    tracks.iter().map(|t| t.id.clone()).collect()
}

/// §1 core acceptance over the real HTTP contract: two casters share a library;
/// a foreign private track is invisible in the list, in the search **and** via a
/// direct URL (`get` → `NotFound`).
#[tokio::test]
async fn http_track_api_enforces_visibility_scope() {
    let s = Scheduler::boot().await;
    let a = s.seed_caster().await;
    let b = s.seed_caster().await;
    let api = s.tracks();

    let shared = api
        .create(
            None,
            &new_track("m-shared", "Shared Song", "shared", Some(&a)),
        )
        .await
        .expect("shared");
    let a_priv = api
        .create(Some(&a), &new_track("m-a", "A Secret", "private", Some(&a)))
        .await
        .expect("a-private");
    let b_priv = api
        .create(Some(&b), &new_track("m-b", "B Secret", "private", Some(&b)))
        .await
        .expect("b-private");

    // A sees shared + own private; B's private is absent.
    let listed = api
        .list(Some(&a), &TrackQuery::default())
        .await
        .expect("list A");
    let seen = ids(&listed);
    assert!(seen.contains(&shared.id));
    assert!(seen.contains(&a_priv.id));
    assert!(
        !seen.contains(&b_priv.id),
        "B-private hidden from A: {seen:?}"
    );

    // No viewer → only shared.
    let anon = api
        .list(None, &TrackQuery::default())
        .await
        .expect("list anon");
    assert_eq!(ids(&anon), vec![shared.id.clone()]);

    // Search for B's private title as A → no hit.
    let q = TrackQuery {
        q: Some("B Secret".into()),
        ..Default::default()
    };
    assert!(
        api.list(Some(&a), &q).await.expect("search").is_empty(),
        "foreign private not searchable"
    );

    // Direct URL: B-private as A → NotFound; as B → Ok.
    let err = api
        .get(Some(&a), &b_priv.id)
        .await
        .expect_err("foreign private");
    assert!(err.is_not_found(), "404 mapped: {err}");
    assert!(api.get(Some(&b), &b_priv.id).await.is_ok());
    // A's private with no viewer → NotFound (never revealed).
    assert!(api.get(None, &a_priv.id).await.unwrap_err().is_not_found());
}

/// Visibility toggle is owner-only: a non-owner patch answers `404` and leaves
/// the row untouched; after the owner flips shared→private it vanishes for the
/// other viewer.
#[tokio::test]
async fn http_track_api_toggle_is_owner_only() {
    let s = Scheduler::boot().await;
    let a = s.seed_caster().await;
    let b = s.seed_caster().await;
    let api = s.tracks();

    let shared = api
        .create(
            None,
            &new_track("m-shared", "Shared Song", "shared", Some(&a)),
        )
        .await
        .expect("shared");

    // B can read the shared track but must not toggle it.
    assert!(api.get(Some(&b), &shared.id).await.is_ok());
    assert!(
        api.set_visibility(Some(&b), &shared.id, "private")
            .await
            .unwrap_err()
            .is_not_found()
    );
    assert_eq!(
        api.get(Some(&b), &shared.id).await.unwrap().visibility,
        "shared",
        "unchanged for non-owner"
    );

    // The owner flips it private; the other viewer loses access by URL.
    let flipped = api
        .set_visibility(Some(&a), &shared.id, "private")
        .await
        .expect("owner toggle");
    assert_eq!(flipped.visibility, "private");
    assert!(
        api.get(Some(&b), &shared.id)
            .await
            .unwrap_err()
            .is_not_found()
    );
}

/// Status mapping of the real endpoints: `409` duplicate → `Conflict`,
/// `422` blank field → `Validation`, `404` unknown delete → `NotFound`.
#[tokio::test]
async fn http_track_api_maps_409_422_404() {
    let s = Scheduler::boot().await;
    let api = s.tracks();

    // 422: blank title.
    let err = api
        .create(None, &new_track("m-bad", "   ", "shared", None))
        .await
        .expect_err("blank title");
    assert!(
        matches!(err, TrackError::Validation { ref field, .. } if field == "title"),
        "422 maps to Validation{{title}}: {err}"
    );

    // 201 then 409 on a duplicate music_api_id.
    api.create(None, &new_track("m-dup", "Song", "shared", None))
        .await
        .expect("first");
    let err = api
        .create(None, &new_track("m-dup", "Song", "shared", None))
        .await
        .expect_err("duplicate");
    assert!(err.is_conflict(), "409 maps to Conflict: {err}");

    // 404: delete an unknown id.
    let err = api
        .delete(None, &new_id())
        .await
        .expect_err("unknown delete");
    assert!(err.is_not_found(), "404 maps to NotFound: {err}");
}

/// Round trip through the real scheduler: create → delete an owned track, and a
/// filtered list narrows by visibility.
#[tokio::test]
async fn http_track_api_crud_roundtrip() {
    let s = Scheduler::boot().await;
    let a = s.seed_caster().await;
    let api = s.tracks();

    let created = api
        .create(Some(&a), &new_track("m-1", "Mine", "private", Some(&a)))
        .await
        .expect("create");
    assert_eq!(created.id.len(), 36, "UUIDv7 text PK");
    assert_eq!(created.visibility, "private");

    let priv_only = TrackQuery {
        visibility: Some("private".into()),
        ..Default::default()
    };
    let listed = api.list(Some(&a), &priv_only).await.expect("filter");
    assert_eq!(ids(&listed), vec![created.id.clone()]);

    api.delete(Some(&a), &created.id).await.expect("delete");
    assert!(
        api.get(Some(&a), &created.id)
            .await
            .unwrap_err()
            .is_not_found()
    );
}
