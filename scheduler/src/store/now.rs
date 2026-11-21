//! Public-safe now-playing composition (§4.2, Issue #107).
//!
//! `GET /now` is the *internal* endpoint the tapes service polls to build the
//! public `/api/v1/public/nowplaying` projection. This module composes it from
//! existing tables only — no writes, no migration, no extra DTO on the playback
//! layer (§4.2):
//!
//! * `layer`/`source` — [`crate::store::control::active_layer`] (K3).
//! * `title`/`artist` — [`crate::store::playback::current`].
//! * `show`/`caster` — the running `broadcasts` row (`status = 'live'`) joined
//!   to `shows` / `casters`.
//! * `started_at` — the running item's `queue_items.played_at`, fallback
//!   `broadcasts.starts_at`.
//! * `live` — [`crate::store::control::live_is_active`] (§2.5).

use sqlx::SqlitePool;

use crate::dto::{NowCasterDto, NowPlayingDto, NowShowDto};
use crate::error::ApiResult;
use crate::store::control::{active_layer, live_is_active};
use crate::store::playback;

/// Composes the public-safe now-playing snapshot (§4.2, Issue #107).
pub async fn now_playing(pool: &SqlitePool) -> ApiResult<NowPlayingDto> {
    let sources = crate::store::sources::list(pool).await?;
    let layer = active_layer(pool, &sources).await?;
    let live = live_is_active(pool).await?;

    // Title/artist come from the running item; never the raw playback DTO
    // (`uri` / `music_api_id` stay internal).
    let item = playback::current(pool).await?;
    let title = item
        .as_ref()
        .map(|item| item.title.clone())
        .filter(|title| !title.is_empty());
    let artist = item
        .as_ref()
        .and_then(|item| item.artist.clone())
        .filter(|artist| !artist.is_empty());

    // The running broadcast resolves show + caster (both optional).
    let broadcast: Option<(Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT show_id, caster_id, starts_at FROM broadcasts \
         WHERE status = 'live' ORDER BY starts_at, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    let (show, caster, broadcast_start) = match broadcast {
        Some((show_id, caster_id, starts_at)) => {
            let show = match show_id.as_deref() {
                Some(id) => lookup_show(pool, id).await?,
                None => None,
            };
            let caster = lookup_caster(pool, &caster_id).await?;
            (show, caster, starts_at)
        }
        None => (None, None, None),
    };

    // Start of the running item, else the broadcast start.
    let started_at = match played_at_of_running_item(pool).await? {
        Some(ts) => Some(ts),
        None => broadcast_start,
    };

    Ok(NowPlayingDto {
        layer,
        source: layer,
        title,
        artist,
        show,
        caster,
        started_at,
        live,
    })
}

/// `played_at` of the running queue item, when present.
async fn played_at_of_running_item(pool: &SqlitePool) -> ApiResult<Option<String>> {
    let ts: Option<String> = sqlx::query_scalar(
        "SELECT played_at FROM queue_items \
         WHERE state = 'playing' AND kind IN ('track', 'tape') AND played_at IS NOT NULL \
         ORDER BY position, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(ts)
}

/// The show's `id` + `name`, or `None` when the referenced show is gone.
async fn lookup_show(pool: &SqlitePool, id: &str) -> ApiResult<Option<NowShowDto>> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT id, name FROM shows WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(id, name)| NowShowDto { id, name }))
}

/// The caster's `id` + `display_name`, or `None` when the caster is gone.
async fn lookup_caster(pool: &SqlitePool, id: &str) -> ApiResult<Option<NowCasterDto>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT id, display_name FROM casters WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id, name)| NowCasterDto { id, name }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::SourceLayer;

    const TS: &str = "2026-11-18T12:00:00.000Z";
    const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";
    const CASTER: &str = "0192aaaa-0001-7000-8000-00000000000a";
    const SHOW: &str = "0192bbbb-0001-7000-8000-00000000000b";
    const TRACK: &str = "0192cccc-0001-7000-8000-00000000000c";
    const QUEUE: &str = "0192dddd-0001-7000-8000-00000000000d";
    const ITEM: &str = "0192eeee-0001-7000-8000-00000000000e";
    const BROADCAST: &str = "0192ffff-0001-7000-8000-00000000000f";
    const PLAYED_AT: &str = "2026-11-18T12:05:00.000Z";

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::connect(&dir.path().join("zukka.db"))
            .await
            .expect("connect + migrate");
        (dir, pool)
    }

    async fn insert_fixtures(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO casters \
             (id, username, display_name, password_hash, role_id, is_active, created_at, updated_at) \
             VALUES (?, 'caster', 'DJ Test', 'hash', ?, 1, ?, ?)",
        )
        .bind(CASTER)
        .bind(ROLE_CASTER)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert caster");

        sqlx::query(
            "INSERT INTO shows (id, slug, name, created_by, created_at, updated_at) \
             VALUES (?, 'test-show', 'Test Show', ?, ?, ?)",
        )
        .bind(SHOW)
        .bind(CASTER)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert show");

        sqlx::query(
            "INSERT INTO tracks \
             (id, music_api_id, title, artist, duration_ms, storage_key, created_at, updated_at) \
             VALUES (?, 'm1', 'A Song', 'An Artist', 215000, 's3/key', ?, ?)",
        )
        .bind(TRACK)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert track");

        sqlx::query(
            "INSERT INTO queues (id, caster_id, name, kind, created_at, updated_at) \
             VALUES (?, ?, 'Main', 'slot', ?, ?)",
        )
        .bind(QUEUE)
        .bind(CASTER)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert queue");

        // A running live broadcast: resolves show + caster, makes `live = true`.
        sqlx::query(
            "INSERT INTO broadcasts \
             (id, show_id, caster_id, kind, title, starts_at, status, created_by, created_at, updated_at) \
             VALUES (?, ?, ?, 'live', 'Live Now', ?, 'live', ?, ?, ?)",
        )
        .bind(BROADCAST)
        .bind(SHOW)
        .bind(CASTER)
        .bind(TS)
        .bind(CASTER)
        .bind(TS)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert broadcast");

        sqlx::query(
            "INSERT INTO queue_items \
             (id, queue_id, position, kind, track_id, state, played_at, created_at) \
             VALUES (?, ?, 0, 'track', ?, 'playing', ?, ?)",
        )
        .bind(ITEM)
        .bind(QUEUE)
        .bind(TRACK)
        .bind(PLAYED_AT)
        .bind(TS)
        .execute(pool)
        .await
        .expect("insert queue item");
    }

    #[tokio::test]
    async fn empty_state_is_all_null_and_not_live() {
        let (_dir, pool) = pool().await;
        let dto = now_playing(&pool).await.expect("now_playing");

        assert_eq!(dto.layer, None);
        assert_eq!(dto.source, None);
        assert_eq!(dto.title, None);
        assert_eq!(dto.artist, None);
        assert_eq!(dto.show, None);
        assert_eq!(dto.caster, None);
        assert_eq!(dto.started_at, None);
        assert!(!dto.live);
    }

    #[tokio::test]
    async fn running_broadcast_and_playing_item_is_fully_populated() {
        let (_dir, pool) = pool().await;
        insert_fixtures(&pool).await;

        let dto = now_playing(&pool).await.expect("now_playing");

        // A running live broadcast activates the live layer.
        assert_eq!(dto.layer, Some(SourceLayer::Live));
        assert_eq!(dto.source, Some(SourceLayer::Live));
        assert!(dto.live);
        assert_eq!(dto.title.as_deref(), Some("A Song"));
        assert_eq!(dto.artist.as_deref(), Some("An Artist"));
        assert_eq!(
            dto.show,
            Some(NowShowDto {
                id: SHOW.to_string(),
                name: "Test Show".to_string(),
            })
        );
        assert_eq!(
            dto.caster,
            Some(NowCasterDto {
                id: CASTER.to_string(),
                name: "DJ Test".to_string(),
            })
        );
        assert_eq!(dto.started_at.as_deref(), Some(PLAYED_AT));
    }

    #[tokio::test]
    async fn started_at_falls_back_to_broadcast_start() {
        let (_dir, pool) = pool().await;
        insert_fixtures(&pool).await;
        // The playing item has no `played_at` yet → broadcast start wins.
        sqlx::query("UPDATE queue_items SET played_at = NULL WHERE id = ?")
            .bind(ITEM)
            .execute(&pool)
            .await
            .expect("clear played_at");

        let dto = now_playing(&pool).await.expect("now_playing");
        assert_eq!(dto.started_at.as_deref(), Some(TS));
    }
}
