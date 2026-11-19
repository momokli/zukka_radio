//! Control store: operator/portal ops on the live layer (Issue #15).
//!
//! ## State model (D1)
//!
//! `live_sessions` does not exist in the frozen `0001_init.sql`; migrations are
//! append-only and finished for M1. Control state therefore lives in the
//! existing `settings` key/value table plus the append-only `event_log`:
//!
//! | `settings.key`            | value                              | meaning                       |
//! | ------------------------- | ---------------------------------- | ----------------------------- |
//! | `live.override_caster_id` | UUIDv7 of the caster               | live override active          |
//! | `control.forced_layer`    | one of the six layers              | forced layer (`switch`)       |
//! | `control.paused`          | `"true"` / `"false"`               | pause flag                    |
//!
//! Clear deletes the row (`settings.value` is `NOT NULL`, an empty string would
//! be ambiguous); set/switch/pause upsert.
//!
//! ## Liquidsoap push is out of scope (§7)
//!
//! The handlers validate, persist state and log; the actual
//! `POST http://liquidsoap:8010/control/*` push is M4/#32 and appears only as a
//! `TODO(#32)` seam below.

use std::collections::BTreeMap;

use serde_json::json;
use sqlx::SqlitePool;

use crate::dto::{
    ConnectedCasterDto, ControlLiveRequest, ControlPauseRequest, ControlSkipRequest,
    ControlStateDto, ControlSwitchRequest, LiveStateDto, SourceDto, SourceLayer,
};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;
use crate::uuid::new_id;

/// `settings` key holding the live-override caster id.
const LIVE_OVERRIDE_KEY: &str = "live.override_caster_id";
/// `settings` key holding the forced layer (`switch`).
const FORCED_LAYER_KEY: &str = "control.forced_layer";
/// `settings` key holding the pause flag.
const PAUSED_KEY: &str = "control.paused";

// ---------------------------------------------------------------------------
// Settings helpers (US-2)
// ---------------------------------------------------------------------------

/// Reads one `settings` value, `None` when the key is absent.
pub(crate) async fn get_setting(pool: &SqlitePool, key: &str) -> ApiResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(value)
}

/// Upserts one `settings` value (`INSERT … ON CONFLICT(key) DO UPDATE …`).
pub(crate) async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> ApiResult<()> {
    let ts = now_ts();
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(&ts)
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes one `settings` key (idempotent no-op when absent).
pub(crate) async fn delete_setting(pool: &SqlitePool, key: &str) -> ApiResult<()> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

/// Appends one control event to the append-only `event_log`.
pub(crate) async fn log_control_event(
    pool: &SqlitePool,
    event: &'static str,
    source_layer: Option<&str>,
    details: serde_json::Value,
) -> ApiResult<()> {
    let id = new_id();
    let ts = now_ts();
    sqlx::query(
        "INSERT INTO event_log \
         (id, occurred_at, event, actor_caster_id, broadcast_id, caster_id, source_layer, details, \
          created_at) \
         VALUES (?, ?, ?, NULL, NULL, NULL, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&ts)
    .bind(event)
    .bind(source_layer)
    .bind(details.to_string())
    .bind(&ts)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// "live active" (US-3, D2)
// ---------------------------------------------------------------------------

/// True when the live layer is (potentially) the output (§2.5).
///
/// Two independent signals, OR-combined (D2): an explicit
/// `live.override_caster_id` (control plane) **or** a running live broadcast
/// (`kind='live' AND status='live'`, data plane). The OR is the stricter
/// reading and can never mute a live caster.
pub async fn live_is_active(pool: &SqlitePool) -> ApiResult<bool> {
    if get_setting(pool, LIVE_OVERRIDE_KEY).await?.is_some() {
        return Ok(true);
    }
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM broadcasts WHERE kind = 'live' AND status = 'live'",
    )
    .fetch_one(pool)
    .await?;
    Ok(live > 0)
}

// ---------------------------------------------------------------------------
// Shared validation
// ---------------------------------------------------------------------------

/// Requires a non-empty `reason`, else `422 validation_error` (D5).
fn require_reason(raw: Option<&str>) -> ApiResult<String> {
    match raw {
        Some(value) if !value.trim().is_empty() => Ok(value.to_string()),
        _ => Err(ApiError::validation("reason", "reason is required")),
    }
}

/// Requires a parseable UUIDv7 caster id (D4/§4.1), else `422`.
fn validate_caster_id(raw: &str) -> ApiResult<()> {
    match uuid::Uuid::parse_str(raw) {
        Ok(id) if id.get_version_num() == 7 => Ok(()),
        Ok(_) => Err(ApiError::validation(
            "caster_id",
            "caster_id must be a UUIDv7",
        )),
        Err(_) => Err(ApiError::validation(
            "caster_id",
            format!("invalid caster_id '{raw}'"),
        )),
    }
}

/// Ensures the referenced caster exists, else `404 not_found`.
async fn ensure_caster(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM casters WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("caster", id));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// `POST /control/live` — set/clear the live override (US-4).
pub async fn live(pool: &SqlitePool, req: &ControlLiveRequest) -> ApiResult<()> {
    let reason = require_reason(req.reason.as_deref())?;
    match req.caster_id.as_deref() {
        // `null` / absent ⇒ clear (DELETE row, D1).
        None => {
            delete_setting(pool, LIVE_OVERRIDE_KEY).await?;
            log_control_event(
                pool,
                "live_override_cleared",
                None,
                json!({ "reason": reason }),
            )
            .await?;
            // TODO(#32): push `live.clear` to http://liquidsoap:8010/control/live.clear (M4).
        }
        // a caster id ⇒ set the override via upsert.
        Some(raw) => {
            validate_caster_id(raw)?;
            ensure_caster(pool, raw).await?;
            set_setting(pool, LIVE_OVERRIDE_KEY, raw).await?;
            log_control_event(
                pool,
                "live_override_set",
                None,
                json!({ "reason": reason, "caster_id": raw }),
            )
            .await?;
            // TODO(#32): push `live.set <mount>` to http://liquidsoap:8010/control/live.set (M4).
        }
    }
    Ok(())
}

/// `POST /control/switch` — force one of the six layers (US-5).
pub async fn switch(pool: &SqlitePool, req: &ControlSwitchRequest) -> ApiResult<()> {
    let reason = require_reason(req.reason.as_deref())?;
    let layer = match req.layer.as_deref() {
        Some(raw) => SourceLayer::parse("layer", raw)?,
        None => return Err(ApiError::validation("layer", "layer is required")),
    };
    set_setting(pool, FORCED_LAYER_KEY, layer.as_str()).await?;
    log_control_event(
        pool,
        "source_switched",
        Some(layer.as_str()),
        json!({ "reason": reason }),
    )
    .await?;
    // TODO(#32): push the layer force to http://liquidsoap:8010/control/... (M4).
    Ok(())
}

/// `POST /control/skip` — skip the playing item, idempotent (US-6).
pub async fn skip(pool: &SqlitePool, req: &ControlSkipRequest) -> ApiResult<()> {
    let reason = require_reason(req.reason.as_deref())?;
    let playing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM queue_items WHERE state = 'playing' ORDER BY position, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    let (skipped, queue_item_id) = match playing {
        Some(id) => {
            sqlx::query(
                "UPDATE queue_items SET state = 'skipped' WHERE id = ? AND state = 'playing'",
            )
            .bind(&id)
            .execute(pool)
            .await?;
            (true, Some(id))
        }
        // no playing item ⇒ idempotent no-op, still `204`.
        None => (false, None),
    };
    log_control_event(
        pool,
        "control_action",
        None,
        json!({
            "action": "skip",
            "reason": reason,
            "skipped": skipped,
            "queue_item_id": queue_item_id,
        }),
    )
    .await?;
    // TODO(#32): push the skip command to http://liquidsoap:8010/control/skip (M4).
    Ok(())
}

// ---------------------------------------------------------------------------
// Read-only state composition (Issue #22, US-1)
// ---------------------------------------------------------------------------

/// Composes the full read-only control-panel state (`GET /control/state`).
///
/// Pure `SELECT`s over existing tables (K1): no writes, no lock beyond reading.
/// `active_layer` is the K3 approximation; `derived` is always `true` (K2).
pub async fn state(pool: &SqlitePool) -> ApiResult<ControlStateDto> {
    let sources = crate::store::sources::list(pool).await?;

    let paused = get_setting(pool, PAUSED_KEY).await?.as_deref() == Some("true");

    // An unparseable setting is treated as absent (a `GET` must never `500`).
    let forced_layer = get_setting(pool, FORCED_LAYER_KEY)
        .await?
        .and_then(|raw| SourceLayer::parse("control.forced_layer", &raw).ok());

    let override_caster_id = get_setting(pool, LIVE_OVERRIDE_KEY).await?;
    let active_caster_id = match override_caster_id.clone() {
        Some(id) => Some(id),
        None => {
            sqlx::query_scalar::<_, String>(
                "SELECT caster_id FROM broadcasts \
                 WHERE kind = 'live' AND status = 'live' ORDER BY starts_at, id LIMIT 1",
            )
            .fetch_optional(pool)
            .await?
        }
    };

    let connected = connected_casters(pool).await?;
    let now_playing = crate::store::playback::current(pool).await?;
    let active_layer = active_layer(pool, &sources).await?;

    Ok(ControlStateDto {
        sources,
        paused,
        forced_layer,
        active_layer,
        derived: true,
        live: LiveStateDto {
            override_caster_id,
            active_caster_id,
            connected,
        },
        now_playing,
    })
}

/// K3 layer derivation (highest first): `forced_layer` → `live` →
/// `show`/`queue` (running broadcast or active queue with a pending item) →
/// `rotation` (content present) → `relay`/`fun` (opt-in settings). Only
/// enabled layers participate; the first matching layer wins.
async fn active_layer(pool: &SqlitePool, sources: &[SourceDto]) -> ApiResult<Option<SourceLayer>> {
    let enabled = |layer: SourceLayer| sources.iter().any(|s| s.name == layer && s.enabled);

    // (1) forced layer always wins.
    if let Some(raw) = get_setting(pool, FORCED_LAYER_KEY).await? {
        if let Ok(layer) = SourceLayer::parse("control.forced_layer", &raw) {
            return Ok(Some(layer));
        }
    }

    // (2) live layer active (override or running live broadcast).
    if enabled(SourceLayer::Live) && live_is_active(pool).await? {
        return Ok(Some(SourceLayer::Live));
    }

    // (3) a running broadcast: `recording` serves the `show` layer, otherwise
    // `playlist`/`live`-kind content comes from the `queue` layer.
    let running_kind: Option<String> = sqlx::query_scalar(
        "SELECT kind FROM broadcasts WHERE status = 'live' ORDER BY starts_at, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    if let Some(kind) = running_kind {
        let layer = if kind == "recording" {
            SourceLayer::Show
        } else {
            SourceLayer::Queue
        };
        if enabled(layer) {
            return Ok(Some(layer));
        }
    }

    // (4) an active queue with a playable pending item.
    if enabled(SourceLayer::Queue) {
        if let Some(queue_id) = crate::store::playback::active_queue_id(pool).await? {
            let pending: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM queue_items \
                 WHERE queue_id = ? AND state = 'pending' AND kind IN ('track', 'tape')",
            )
            .bind(&queue_id)
            .fetch_one(pool)
            .await?;
            if pending > 0 {
                return Ok(Some(SourceLayer::Queue));
            }
        }
    }

    // (5) rotation: satisfied once the shared library has playable content.
    if enabled(SourceLayer::Rotation) {
        let shared: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tracks WHERE visibility = 'shared' AND music_api_id <> ''",
        )
        .fetch_one(pool)
        .await?;
        if shared > 0 {
            return Ok(Some(SourceLayer::Rotation));
        }
    }

    // (6) relay / fun: no signal exists yet (#32); opt-in settings keys only.
    if enabled(SourceLayer::Relay)
        && get_setting(pool, "control.relay_active").await?.as_deref() == Some("true")
    {
        return Ok(Some(SourceLayer::Relay));
    }
    if enabled(SourceLayer::Fun)
        && get_setting(pool, "control.fun_active").await?.as_deref() == Some("true")
    {
        return Ok(Some(SourceLayer::Fun));
    }

    Ok(None)
}

/// K4: casters whose latest `event_log` live event is `live_connected`.
///
/// There is no `live_sessions` table (migrations are frozen), so connection
/// state is recomputed from the append-only log: per `caster_id`, the latest
/// `live_connected`/`live_disconnected` decides. Sorted by `caster_id`; empty
/// when no events exist (no error).
async fn connected_casters(pool: &SqlitePool) -> ApiResult<Vec<ConnectedCasterDto>> {
    let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT e.caster_id, e.event, c.harbor_mount, e.occurred_at \
         FROM event_log e LEFT JOIN casters c ON c.id = e.caster_id \
         WHERE e.event IN ('live_connected', 'live_disconnected') \
           AND e.caster_id IS NOT NULL \
         ORDER BY e.occurred_at DESC, e.id DESC",
    )
    .fetch_all(pool)
    .await?;

    // BTreeMap keeps the result sorted by `caster_id`; `or_insert` keeps only
    // the newest event per caster (the rows are already newest-first).
    let mut latest: BTreeMap<String, (String, Option<String>, String)> = BTreeMap::new();
    for (caster_id, event, mount, occurred_at) in rows {
        latest
            .entry(caster_id)
            .or_insert((event, mount, occurred_at));
    }
    Ok(latest
        .into_iter()
        .filter(|(_, (event, _, _))| event == "live_connected")
        .map(|(caster_id, (_, mount, since))| ConnectedCasterDto {
            caster_id,
            mount,
            since,
        })
        .collect())
}

/// `POST /control/pause` — pause/resume with the live guard (US-7, D3).
pub async fn pause(pool: &SqlitePool, req: &ControlPauseRequest) -> ApiResult<()> {
    let reason = require_reason(req.reason.as_deref())?;
    let paused = match &req.paused {
        Some(serde_json::Value::Bool(value)) => *value,
        _ => {
            return Err(ApiError::validation(
                "paused",
                "paused must be a JSON boolean",
            ));
        }
    };

    // Guard only on pause: resuming is always allowed (idempotent, D3).
    if paused && live_is_active(pool).await? {
        return Err(ApiError::Conflict {
            code: "live_active",
            message: "cannot pause active live layer".to_string(),
            details: Some(json!({ "field": "paused" })),
        });
    }

    set_setting(pool, PAUSED_KEY, if paused { "true" } else { "false" }).await?;
    log_control_event(
        pool,
        "control_action",
        None,
        json!({
            "action": if paused { "pause" } else { "resume" },
            "reason": reason,
        }),
    )
    .await?;
    // TODO(#32): push `pause`/`resume` to http://liquidsoap:8010/control/... (M4).
    Ok(())
}
