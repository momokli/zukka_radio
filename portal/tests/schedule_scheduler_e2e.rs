//! Cross-crate end-to-end tests: the portal's [`HttpShowApi`],
//! [`HttpBroadcastApi`] and [`HttpScheduleApi`] against the **real** scheduler
//! (Issue #21).
//!
//! `zukka_scheduler::api::router` is served on an ephemeral TCP port over a
//! migrated temporary SQLite database, and the portal's production clients talk
//! to it. This exercises the true portal↔scheduler contract: status mapping
//! (`404`/`409`/`422`), the weekly recurrence and the DST fall-back rule (D13).

use chrono::DateTime;
use chrono_tz::Europe::Berlin;
use sqlx::SqlitePool;
use tempfile::TempDir;

use zukka_portal::api_client::{
    BroadcastApi, HttpBroadcastApi, HttpScheduleApi, HttpShowApi, NewBroadcast, NewShow,
    ScheduleApi, ScheduleError, ShowApi,
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

    fn shows(&self) -> HttpShowApi {
        HttpShowApi::new(self.base_url.clone(), "svc-token").expect("show client")
    }

    fn broadcasts(&self) -> HttpBroadcastApi {
        HttpBroadcastApi::new(self.base_url.clone(), "svc-token").expect("broadcast client")
    }

    fn schedule(&self) -> HttpScheduleApi {
        HttpScheduleApi::new(self.base_url.clone(), "svc-token").expect("schedule client")
    }
}

fn berlin_hm(raw: &str) -> String {
    DateTime::parse_from_rfc3339(raw)
        .expect("rfc3339")
        .with_timezone(&Berlin)
        .format("%H:%M")
        .to_string()
}

/// Show + broadcast + schedule window + delete through the real scheduler.
#[tokio::test]
async fn http_show_broadcast_schedule_roundtrip() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;

    let show = s
        .shows()
        .create(
            Some(&caster),
            &NewShow {
                slug: "morning".into(),
                name: "Morning".into(),
                caster_id: Some(caster.clone()),
                created_by: caster.clone(),
                ..Default::default()
            },
        )
        .await
        .expect("create show");
    assert_eq!(show.name, "Morning");

    let broadcast = s
        .broadcasts()
        .create(
            Some(&caster),
            &NewBroadcast {
                caster_id: caster.clone(),
                show_id: Some(show.id.clone()),
                kind: "live".into(),
                title: "Live Now".into(),
                starts_at: "2026-11-18T20:00:00+01:00".into(),
                ..Default::default()
            },
        )
        .await
        .expect("create broadcast");
    assert_eq!(broadcast.version, 1);
    assert!(broadcast.warnings.is_empty());

    let fetched = s
        .broadcasts()
        .get(Some(&caster), &broadcast.id)
        .await
        .expect("get broadcast");
    assert_eq!(fetched.title, "Live Now");

    let entries = s
        .schedule()
        .window(
            Some(&caster),
            "2026-11-17T00:00:00Z",
            "2026-11-20T00:00:00Z",
        )
        .await
        .expect("window");
    assert!(
        entries.iter().any(|e| e.title == "Live Now"),
        "persisted broadcast listed: {entries:?}"
    );

    s.shows()
        .list(Some(&caster), None)
        .await
        .expect("list shows");
    s.broadcasts()
        .delete(Some(&caster), &broadcast.id)
        .await
        .expect("delete");
    assert!(
        s.broadcasts()
            .get(Some(&caster), &broadcast.id)
            .await
            .unwrap_err()
            .is_not_found()
    );
}

/// D13 proof: a weekly show anchored in Berlin keeps the **same local wall
/// time** across the fall-back DST transition (expanded by the scheduler).
#[tokio::test]
async fn weekly_show_keeps_local_time_across_dst() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;

    // Wednesdays 20:00 Berlin. 2026-10-25 is the fall-back Sunday.
    let rule = "DTSTART=20261014T200000;FREQ=WEEKLY;INTERVAL=1;DURATION=PT2H";
    s.shows()
        .create(
            Some(&caster),
            &NewShow {
                slug: "dst".into(),
                name: "DST Show".into(),
                recurrence: Some(rule.into()),
                caster_id: Some(caster.clone()),
                created_by: caster.clone(),
                ..Default::default()
            },
        )
        .await
        .expect("create show");

    let entries = s
        .schedule()
        .window(
            Some(&caster),
            "2026-10-12T00:00:00Z",
            "2026-11-01T00:00:00Z",
        )
        .await
        .expect("window");
    let dst_show: Vec<_> = entries
        .iter()
        .filter(|e| e.title == "DST Show")
        .map(|e| berlin_hm(&e.starts_at))
        .collect();
    assert!(dst_show.len() >= 2, "several occurrences: {dst_show:?}");
    assert!(
        dst_show.iter().all(|hm| hm == "20:00"),
        "local time stable across DST: {dst_show:?}"
    );
    // Prove the two sides of the transition really differ in UTC offset.
    let offsets: Vec<String> = entries
        .iter()
        .filter(|e| e.title == "DST Show")
        .filter_map(|e| e.starts_at.get(e.starts_at.len().saturating_sub(6)..))
        .map(str::to_string)
        .collect();
    assert!(
        offsets.iter().any(|o| o.contains("+02:00")),
        "summer offset present: {offsets:?}"
    );
    assert!(
        offsets.iter().any(|o| o.contains("+01:00")),
        "winter offset present: {offsets:?}"
    );
}

/// Status mapping: `422` blank name / invalid recurrence, `404` unknown show.
#[tokio::test]
async fn http_show_api_maps_422_and_404() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;

    let err = s
        .shows()
        .create(
            Some(&caster),
            &NewShow {
                slug: "   ".into(),
                name: "X".into(),
                created_by: caster.clone(),
                ..Default::default()
            },
        )
        .await
        .expect_err("blank slug");
    assert!(
        matches!(err, ScheduleError::Validation { ref field, .. } if field == "slug"),
        "422 maps to Validation{{slug}}: {err}"
    );

    let err = s
        .shows()
        .create(
            Some(&caster),
            &NewShow {
                slug: "bad".into(),
                name: "Bad".into(),
                recurrence: Some("FREQ=WEEKLY".into()),
                created_by: caster.clone(),
                ..Default::default()
            },
        )
        .await
        .expect_err("invalid recurrence");
    assert!(
        matches!(err, ScheduleError::Validation { ref field, .. } if field == "recurrence"),
        "422 maps to Validation{{recurrence}}: {err}"
    );

    let err = s
        .shows()
        .get(Some(&caster), &new_id())
        .await
        .expect_err("unknown");
    assert!(err.is_not_found(), "404 maps to NotFound: {err}");
}

/// Hard conflicts: `end < start` is a `422`, a self-overlapping show a `409`.
#[tokio::test]
async fn http_broadcast_api_maps_422_and_409() {
    let s = Scheduler::boot().await;
    let caster = s.seed_caster().await;

    let show = s
        .shows()
        .create(
            Some(&caster),
            &NewShow {
                slug: "weekly".into(),
                name: "Weekly".into(),
                caster_id: Some(caster.clone()),
                created_by: caster.clone(),
                ..Default::default()
            },
        )
        .await
        .expect("create show");

    let err = s
        .broadcasts()
        .create(
            Some(&caster),
            &NewBroadcast {
                caster_id: caster.clone(),
                kind: "live".into(),
                title: "Backwards".into(),
                starts_at: "2026-11-18T22:00:00+01:00".into(),
                ends_at: Some("2026-11-18T20:00:00+01:00".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("end before start");
    assert!(
        matches!(err, ScheduleError::Validation { ref field, .. } if field == "ends_at"),
        "422 maps to Validation{{ends_at}}: {err}"
    );

    s.broadcasts()
        .create(
            Some(&caster),
            &NewBroadcast {
                caster_id: caster.clone(),
                show_id: Some(show.id.clone()),
                kind: "live".into(),
                title: "First".into(),
                starts_at: "2026-11-18T20:00:00+01:00".into(),
                ends_at: Some("2026-11-18T22:00:00+01:00".into()),
                ..Default::default()
            },
        )
        .await
        .expect("first");

    let err = s
        .broadcasts()
        .create(
            Some(&caster),
            &NewBroadcast {
                caster_id: caster.clone(),
                show_id: Some(show.id.clone()),
                kind: "live".into(),
                title: "Second".into(),
                starts_at: "2026-11-18T21:00:00+01:00".into(),
                ends_at: Some("2026-11-18T23:00:00+01:00".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("self overlap");
    assert_eq!(
        err.conflict_code(),
        Some("show_overlap"),
        "409 maps to Conflict{{show_overlap}}: {err}"
    );
}
