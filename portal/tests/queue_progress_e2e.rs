//! Cross-crate end-to-end test: the **Ingest → Queue → Broadcast → Playback**
//! seam (Issue #122).
//!
//! A track is ingested via the portal's Music-API client ([`FakeMusicApi`], the
//! in-process `dyn MusicApi`) and then registered in the scheduler's library
//! through [`HttpTrackApi`]. The library track is placed in a `slot` queue via
//! [`HttpQueueApi`], a `playlist` broadcast is created + started via
//! [`HttpBroadcastApi`], and the full `next → started → current → ended → next`
//! lifecycle is walked over the **real** `/playback/*` HTTP routes with the
//! `x-liquidsoap-token` header (proves #118 playback reporting + #119 broadcast
//! start together).
//!
//! Unlike `scheduler/tests/playback_e2e.rs` (which seeds caster/track/queue via
//! direct SQL), the queue content here arrives through the ingested track — the
//! scheduler is served on an ephemeral TCP port and the portal's production
//! clients talk to it, mirroring `portal/tests/queue_scheduler_e2e.rs`.

use std::sync::OnceLock;

use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_portal::api_client::{
    BroadcastApi, HttpBroadcastApi, HttpQueueApi, HttpTrackApi, NewBroadcast, NewQueue,
    NewQueueItem, NewTrackRequest, QueueApi, TrackApi,
};
use zukka_portal::music_api::{FakeMusicApi, MusicApi, NewTrack};

use zukka_scheduler::api::auth as sched_auth;
use zukka_scheduler::uuid::new_id;

const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
const TS: &str = "2026-11-21T12:00:00.000Z";
const LIQUIDSOAP_TOKEN: &str = "e2e-liquidsoap-token";
/// Any non-empty service token: the public `/api/v1/*` routes are not
/// service-token guarded (only `/internal/*` is).
const SERVICE_TOKEN: &str = "e2e-service-token";

/// Sets `LIQUIDSOAP_TOKEN` exactly once for the whole test binary, before any
/// playback request is sent (edition 2024: `set_var` is unsafe).
fn set_liquidsoap_token() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: fixed constant, funneled through `OnceLock`, so no other
        // thread observes a torn environment here.
        unsafe { std::env::set_var(sched_auth::TOKEN_ENV, LIQUIDSOAP_TOKEN) };
    });
}

/// A live scheduler on an ephemeral port plus the migrated database behind it.
struct Scheduler {
    _dir: TempDir,
    base_url: String,
    pool: SqlitePool,
}

impl Scheduler {
    async fn boot() -> Self {
        set_liquidsoap_token();
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

    /// Seeds a caster (needed because `queues.caster_id` / `tracks.caster_id`
    /// are FKs).
    async fn seed_caster(&self) -> String {
        let id = new_id();
        sqlx::query(
            "INSERT INTO casters (id, username, display_name, password_hash, role_id, is_active, \
             created_at, updated_at) VALUES (?, ?, 'E2E Caster', 'h', ?, 1, ?, ?)",
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
        HttpTrackApi::new(self.base_url.clone(), SERVICE_TOKEN).expect("track client")
    }

    fn queues(&self) -> HttpQueueApi {
        HttpQueueApi::new(self.base_url.clone(), SERVICE_TOKEN).expect("queue client")
    }

    fn broadcasts(&self) -> HttpBroadcastApi {
        HttpBroadcastApi::new(self.base_url.clone(), SERVICE_TOKEN).expect("broadcast client")
    }
}

/// Sends a playback request with the Liquidsoap token header and decodes the
/// JSON body (`None` for an empty body).
async fn playback(
    client: &reqwest::Client,
    base_url: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (reqwest::StatusCode, Option<Value>) {
    let mut req = client
        .request(method, format!("{base_url}/api/v1{path}"))
        .header("x-liquidsoap-token", LIQUIDSOAP_TOKEN);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.expect("playback request");
    let status = resp.status();
    let text = resp.text().await.expect("playback body");
    let value = if text.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&text).expect("playback json"))
    };
    (status, value)
}

/// The #122 seam end to end: fake Music-API ingest → library track → `slot`
/// queue → live `playlist` broadcast → `next/started/current/ended/next` with
/// the first item ending up `played`.
#[tokio::test]
async fn e2e_queue_progress_ingest_to_played() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;

    // 1) Ingest: the Music API is the file authority; the portal registers the
    //    served track locally afterwards (terminal step of the upload pipeline).
    let music = FakeMusicApi::new();
    let created = music
        .create_track(&NewTrack {
            title: "Ingested Song".into(),
            artist: Some("Artist A".into()),
            sha256: Some("abc123".into()),
        })
        .await
        .expect("fake music api create_track");
    assert_eq!(music.created_count(), 1);

    let track = s
        .tracks()
        .create(
            Some(&caster),
            &NewTrackRequest {
                music_api_id: created.id.clone(),
                title: created.title.clone(),
                storage_key: "s3/ingested-song.flac".into(),
                duration_ms: 215_000,
                artist: created.artist.clone(),
                visibility: Some("shared".into()),
                caster_id: Some(caster.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("register track");
    assert_eq!(track.music_api_id, created.id);
    assert_eq!(track.title, "Ingested Song");

    // 2) Queue (kind `slot`) with two ingested track items.
    let queue = s
        .queues()
        .create(
            Some(&caster),
            &NewQueue {
                caster_id: caster.clone(),
                name: "E2E Slot".into(),
                kind: Some("slot".into()),
                ..Default::default()
            },
        )
        .await
        .expect("create queue");
    assert_eq!(queue.kind, "slot");

    let i1 = s
        .queues()
        .add_item(
            Some(&caster),
            &queue.id,
            &NewQueueItem {
                kind: "track".into(),
                track_id: Some(track.id.clone()),
                position: Some(0),
                ..Default::default()
            },
        )
        .await
        .expect("i1");
    let i2 = s
        .queues()
        .add_item(
            Some(&caster),
            &queue.id,
            &NewQueueItem {
                kind: "track".into(),
                track_id: Some(track.id.clone()),
                position: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("i2");
    assert_eq!(i1.position, 0);
    assert_eq!(i2.position, 1);
    assert_eq!(i1.state, "pending");
    assert_eq!(i2.state, "pending");

    // 3) Broadcast: a live `playlist` broadcast makes `layer=queue` resolve to
    //    this queue (§2.2 priority chain).
    let broadcast = s
        .broadcasts()
        .create(
            Some(&caster),
            &NewBroadcast {
                caster_id: caster.clone(),
                kind: "playlist".into(),
                title: "E2E Show".into(),
                starts_at: TS.into(),
                queue_id: Some(queue.id.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("create broadcast");
    let live = s
        .broadcasts()
        .start_broadcast(Some(&caster), &broadcast.id)
        .await
        .expect("start broadcast");
    assert_eq!(live.status, "live");

    // 4) Playback over the real routes, Liquidsoap-authenticated.
    let client = reqwest::Client::new();

    // next → i1
    let (status, body) = playback(
        &client,
        &s.base_url,
        reqwest::Method::GET,
        "/playback/next?layer=queue",
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "next #1: {body:?}");
    let body = body.expect("next body");
    assert_eq!(body["queue_item_id"], i1.id);
    assert_eq!(body["kind"], "track");
    assert_eq!(body["title"], "Ingested Song");

    // started (i1) → 204; current → i1
    let (status, _) = playback(
        &client,
        &s.base_url,
        reqwest::Method::POST,
        "/playback/started",
        Some(serde_json::json!({ "queue_item_id": i1.id, "kind": "track", "layer": "queue" })),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "started");

    let (status, body) = playback(
        &client,
        &s.base_url,
        reqwest::Method::GET,
        "/playback/current",
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "current: {body:?}");
    assert_eq!(body.expect("current body")["queue_item_id"], i1.id);

    // ended (i1) → 204
    let (status, _) = playback(
        &client,
        &s.base_url,
        reqwest::Method::POST,
        "/playback/ended",
        Some(serde_json::json!({ "queue_item_id": i1.id })),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "ended");

    // next → i2 (the next pending item advances)
    let (status, body) = playback(
        &client,
        &s.base_url,
        reqwest::Method::GET,
        "/playback/next?layer=queue",
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "next #2: {body:?}");
    assert_eq!(body.expect("next #2 body")["queue_item_id"], i2.id);

    // 5) Queue state via the production client: i1 played (+played_at), i2 pending.
    let items = s
        .queues()
        .items(Some(&caster), &queue.id)
        .await
        .expect("queue items");
    let first = items.iter().find(|it| it.id == i1.id).expect("i1 present");
    let second = items.iter().find(|it| it.id == i2.id).expect("i2 present");
    assert_eq!(first.state, "played", "i1 advanced to played");
    assert!(first.played_at.is_some(), "i1 has played_at");
    assert_eq!(second.state, "pending", "i2 untouched");
}
