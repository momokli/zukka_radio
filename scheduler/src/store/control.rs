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
//! ## Liquidsoap push (issue #32)
//!
//! `live` set/clear persist state and then push `live.set <mount>` / `live.clear`
//! to the Liquidsoap command server (`crate::liquidsoap`). The push is
//! best-effort: the DB decision is authoritative and `reassert_live` replays it
//! at boot. `switch`/`skip`/`pause` have no matching §4.4 command and are
//! enforced downstream.

use std::collections::BTreeMap;

use serde_json::json;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::dto::{
    ConnectedCasterDto, ControlLiveRequest, ControlPauseRequest, ControlPlanPutRequest,
    ControlSkipRequest, ControlStateDto, ControlSwitchRequest, LayerSource, LiveStateDto,
    ObservedNowPlaying, PlanDto, PlanEngineStatus, SourceDto, SourceLayer,
};
use crate::error::{ApiError, ApiResult};
use crate::liquidsoap::push_best_effort;
use crate::store::now_ts;
use crate::uuid::new_id;

/// `settings` key holding the live-override caster id.
const LIVE_OVERRIDE_KEY: &str = "live.override_caster_id";
/// `settings` key holding the forced layer (`switch`).
const FORCED_LAYER_KEY: &str = "control.forced_layer";
/// `settings` key holding the pause flag.
const PAUSED_KEY: &str = "control.paused";
/// `settings` key holding the active mixer preset (`default|live|relay-only`).
const PRESET_KEY: &str = "control.preset";

/// The three documented mixer presets (`mixer.liq` `preset_names`).
const PRESETS: [&str; 3] = ["default", "live", "relay-only"];

/// True when `raw` is a documented preset name.
fn is_valid_preset(raw: &str) -> bool {
    PRESETS.contains(&raw)
}

// ---------------------------------------------------------------------------
// Settings helpers (US-2)
// ---------------------------------------------------------------------------

/// Reads one `settings` value, `None` when the key is absent.
pub(crate) async fn get_setting(pool: &SqlitePool, key: &str) -> ApiResult<Option<String>> {
    let mut conn = pool.acquire().await?;
    get_setting_conn(&mut conn, key).await
}

/// Reads one `settings` value on a caller-supplied connection (pool or tx).
pub(crate) async fn get_setting_conn(
    conn: &mut SqliteConnection,
    key: &str,
) -> ApiResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(value)
}

/// Upserts one `settings` value (`INSERT … ON CONFLICT(key) DO UPDATE …`).
pub(crate) async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> ApiResult<()> {
    let mut conn = pool.acquire().await?;
    set_setting_conn(&mut conn, key, value).await
}

/// Upserts one `settings` value on a caller-supplied connection (pool or tx).
pub(crate) async fn set_setting_conn(
    conn: &mut SqliteConnection,
    key: &str,
    value: &str,
) -> ApiResult<()> {
    let ts = now_ts();
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(&ts)
    .execute(&mut *conn)
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

/// Number of running live sources (#42 Deploy-Gate, §2.9).
///
/// `broadcasts` with `kind='live' AND status='live'` **plus** one when the
/// `live.override_caster_id` override is set. Unlike [`live_is_active`] this
/// returns a count, not a bool; only `== 0` is semantically relevant to the
/// gate, the value itself is exposed for observability.
pub async fn live_source_count(pool: &SqlitePool) -> ApiResult<u64> {
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM broadcasts WHERE kind = 'live' AND status = 'live'",
    )
    .fetch_one(pool)
    .await?;
    let override_active = get_setting(pool, LIVE_OVERRIDE_KEY).await?.is_some();
    Ok(live.max(0) as u64 + u64::from(override_active))
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

/// Harbor mount of a caster (`casters.harbor_mount`, §2.5).
async fn harbor_mount(pool: &SqlitePool, id: &str) -> ApiResult<Option<String>> {
    let mount: Option<String> = sqlx::query_scalar("SELECT harbor_mount FROM casters WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(mount.filter(|value| !value.trim().is_empty()))
}

// (removed) local `push_liquidsoap` — moved to `liquidsoap::push_best_effort`
// so the broadcast transition wiring can reuse it (issue #108, US7).

/// Re-asserts a persisted live override after a scheduler restart (§2.5/C43).
///
/// A missing endpoint or a caster that no longer has a mount is a warning, not
/// a boot failure: FCFS remains a valid fallback.
pub async fn reassert_live(pool: &SqlitePool) -> ApiResult<()> {
    let Some(caster_id) = get_setting(pool, LIVE_OVERRIDE_KEY).await? else {
        return Ok(());
    };
    match harbor_mount(pool, &caster_id).await? {
        Some(mount) => {
            crate::liquidsoap::push_best_effort(&crate::liquidsoap::live_set(&mount)).await
        }
        None => tracing::warn!(
            caster_id,
            "live override persisted but caster has no harbor mount"
        ),
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
            push_best_effort(crate::liquidsoap::LIVE_CLEAR).await;
        }
        // a caster id ⇒ set the override via upsert and force its mount now.
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
            // The DB decision stands even when the push fails; `reassert_live`
            // replays it at the next scheduler start (§2.5/C43).
            match harbor_mount(pool, raw).await? {
                Some(mount) => push_best_effort(&crate::liquidsoap::live_set(&mount)).await,
                None => {
                    tracing::warn!(caster_id = raw, "live override: caster has no harbor mount")
                }
            }
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
    // Issue #147: replay the decision to the engine (`layer.force`). The DB
    // stays authoritative; a failed push is re-asserted by `reconcile` at boot.
    push_best_effort(&crate::liquidsoap::layer_force(layer.as_str())).await;
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
    // Skip is applied through the playback API; §4.4 has no control.skip command.
    Ok(())
}

// ---------------------------------------------------------------------------
// Read-only state composition (Issue #22, US-1)
// ---------------------------------------------------------------------------

/// Composes the full read-only control-panel state (`GET /control/state`).
///
/// Pure `SELECT`s over existing tables (K1): no writes, no lock beyond reading.
/// `active_layer` is the effective layer — the observed Icecast layer when one
/// can be derived, else the K3 approximation. `derived` stays `true` forever
/// (compatibility, #22); the origin is exposed via `active_layer_source`.
///
/// Observation is fail-open: an unreachable Icecast never makes `state()`
/// fail (Issue #140) — `active_layer` then simply falls back to `derived`.
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

    // (a) the existing K3 derivation from the scheduler DB.
    let derived_layer = active_layer(pool, &sources).await?;

    // (b) the observed layer from Icecast (fail-open, never an Err).
    let hints = observation_hints(pool, now_playing.as_ref()).await?;
    let snapshot = crate::icecast::observe_icecast().await;
    let observed = crate::icecast::derive_observed_layer(snapshot.as_ref(), &hints);

    let effective_layer = observed.layer.or(derived_layer);
    let active_layer_source = if observed.layer.is_some() {
        Some(LayerSource::Observed)
    } else if derived_layer.is_some() {
        Some(LayerSource::Derived)
    } else {
        None
    };

    Ok(ControlStateDto {
        sources,
        paused,
        forced_layer,
        active_layer: effective_layer,
        derived: true,
        live: LiveStateDto {
            override_caster_id,
            active_caster_id,
            connected,
        },
        now_playing,
        observed_layer: observed.layer,
        derived_layer,
        active_layer_source,
        observed: observed.layer.is_some(),
        observed_reason: Some(observed.reason.to_string()),
        on_air: observed.on_air,
        observed_now_playing: observed.now_playing,
    })
}

/// Gathers the local signals [`crate::icecast::derive_observed_layer`] may use.
async fn observation_hints(
    pool: &SqlitePool,
    now_playing: Option<&crate::dto::PlaybackItemDto>,
) -> ApiResult<crate::icecast::ObservedHints> {
    let db_now_playing = now_playing.map(|item| ObservedNowPlaying {
        title: Some(item.title.clone()),
        artist: item.artist.clone(),
        source: Some("db".to_string()),
    });

    let relay_active_setting =
        get_setting(pool, "control.relay_active").await?.as_deref() == Some("true");

    let running_kind: Option<String> = sqlx::query_scalar(
        "SELECT kind FROM broadcasts WHERE status = 'live' ORDER BY starts_at, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    // Bounded read: title matching only needs a representative slice.
    let shared_titles: Vec<String> = sqlx::query_scalar(
        "SELECT title FROM tracks WHERE visibility = 'shared' ORDER BY id LIMIT 2000",
    )
    .fetch_all(pool)
    .await?;

    Ok(crate::icecast::ObservedHints {
        db_now_playing,
        live_active: live_is_active(pool).await?,
        relay_active_setting,
        running_kind,
        shared_titles,
    })
}

/// K3 layer derivation (highest first): `forced_layer` → `live` →
/// `show`/`queue` (running broadcast or active queue with a pending item) →
/// `rotation` (content present) → `relay`/`fun` (opt-in settings). Only
/// enabled layers participate; the first matching layer wins.
pub(crate) async fn active_layer(
    pool: &SqlitePool,
    sources: &[SourceDto],
) -> ApiResult<Option<SourceLayer>> {
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
            "SELECT COUNT(*) FROM tracks WHERE visibility = 'shared' \
               AND (COALESCE(isrc, '') <> '' OR music_api_id <> '')",
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
    // Pause/resume is enforced downstream; §4.4 has no control.pause command.
    Ok(())
}

// ---------------------------------------------------------------------------
// Issue #147: control plan (presets / force / pause) + engine reconcile
// ---------------------------------------------------------------------------

/// Read helper: the persisted [`PlanDto`] (Issue #147, US-3).
///
/// An unparseable/unknown key is treated as absent (a `GET` must never `500`).
pub async fn get_plan(pool: &SqlitePool) -> ApiResult<PlanDto> {
    let preset = get_setting(pool, PRESET_KEY)
        .await?
        .filter(|raw| is_valid_preset(raw));
    let forced_layer = get_setting(pool, FORCED_LAYER_KEY)
        .await?
        .and_then(|raw| SourceLayer::parse("control.forced_layer", &raw).ok());
    let paused = get_setting(pool, PAUSED_KEY).await?.as_deref() == Some("true");
    let updated_at: Option<String> =
        sqlx::query_scalar("SELECT MAX(updated_at) FROM settings WHERE key IN (?, ?, ?)")
            .bind(PRESET_KEY)
            .bind(FORCED_LAYER_KEY)
            .bind(PAUSED_KEY)
            .fetch_one(pool)
            .await?;

    Ok(PlanDto {
        preset,
        forced_layer,
        paused,
        updated_at,
    })
}

/// Outcome of a best-effort engine push, for the `engine` status object (Q4).
enum PushOutcome {
    Applied,
    Skipped,
    Failed,
}

impl PushOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            PushOutcome::Applied => "applied",
            PushOutcome::Skipped => "skipped",
            PushOutcome::Failed => "failed",
        }
    }
}

/// Best-effort push that also classifies the engine response (no logging side
/// effect beyond `push_best_effort`).
async fn push_outcome(command: &str) -> PushOutcome {
    match crate::liquidsoap::push(command).await {
        Ok(None) => PushOutcome::Skipped,
        Ok(Some(body)) => match crate::liquidsoap::parse_engine_error(&body) {
            None => PushOutcome::Applied,
            Some(code) => {
                tracing::warn!(
                    command,
                    code = code.as_str(),
                    "engine rejected plan command"
                );
                PushOutcome::Failed
            }
        },
        Err(err) => {
            tracing::warn!(command, "engine push failed: {err}");
            PushOutcome::Failed
        }
    }
}

/// `PUT /control/plan` — merge-semantics plan update (Issue #147, US-4).
///
/// Persists the change, audits exactly one `control_action` row and pushes the
/// resulting intent to the engine best-effort. Push failures are **not** fatal:
/// the plan is DB-authoritative and `reconcile` replays it at boot (Q4).
pub async fn put_plan(
    pool: &SqlitePool,
    req: &ControlPlanPutRequest,
) -> ApiResult<(PlanDto, PlanEngineStatus)> {
    let reason = require_reason(req.reason.as_deref())?;

    // Validate every field before writing anything.
    let preset = match &req.preset {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => {
            if !is_valid_preset(raw) {
                return Err(ApiError::validation(
                    "preset",
                    format!("unknown preset '{raw}', expected 'default', 'live' or 'relay-only'"),
                ));
            }
            Some(Some(raw.clone()))
        }
    };
    let forced_layer = match &req.forced_layer {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => Some(Some(SourceLayer::parse("forced_layer", raw)?)),
    };
    let paused = match &req.paused {
        None => None,
        Some(serde_json::Value::Bool(value)) => Some(*value),
        Some(_) => {
            return Err(ApiError::validation(
                "paused",
                "paused must be a JSON boolean",
            ));
        }
    };

    // Guard only on pause: resuming is always allowed (D3, unchanged).
    if paused == Some(true) && live_is_active(pool).await? {
        return Err(ApiError::Conflict {
            code: "live_active",
            message: "cannot pause active live layer".to_string(),
            details: Some(json!({ "field": "paused" })),
        });
    }

    let mut engine = PlanEngineStatus {
        preset: None,
        forced_layer: None,
        paused: "advisory".to_string(),
    };
    let mut changed = serde_json::Map::new();

    match preset {
        None => {}
        Some(None) => {
            delete_setting(pool, PRESET_KEY).await?;
            changed.insert("preset".to_string(), serde_json::Value::Null);
        }
        Some(Some(name)) => {
            set_setting(pool, PRESET_KEY, &name).await?;
            changed.insert("preset".to_string(), json!(name));
            engine.preset = Some(
                push_outcome(&crate::liquidsoap::layer_preset(&name))
                    .await
                    .as_str()
                    .to_string(),
            );
        }
    }

    match forced_layer {
        None => {}
        Some(None) => {
            delete_setting(pool, FORCED_LAYER_KEY).await?;
            changed.insert("forced_layer".to_string(), serde_json::Value::Null);
            engine.forced_layer = Some(
                push_outcome(&crate::liquidsoap::layer_force("none"))
                    .await
                    .as_str()
                    .to_string(),
            );
        }
        Some(Some(layer)) => {
            set_setting(pool, FORCED_LAYER_KEY, layer.as_str()).await?;
            changed.insert("forced_layer".to_string(), json!(layer.as_str()));
            engine.forced_layer = Some(
                push_outcome(&crate::liquidsoap::layer_force(layer.as_str()))
                    .await
                    .as_str()
                    .to_string(),
            );
        }
    }

    if let Some(value) = paused {
        set_setting(pool, PAUSED_KEY, if value { "true" } else { "false" }).await?;
        changed.insert("paused".to_string(), json!(value));
        // No engine command exists for pause; keep the flag advisory (Q5).
        tracing::info!(
            paused = value,
            "control plan pause is advisory (no engine command)"
        );
    }

    log_control_event(
        pool,
        "control_action",
        None,
        json!({ "action": "plan", "reason": reason, "changed": changed }),
    )
    .await?;

    let plan = get_plan(pool).await?;
    Ok((plan, engine))
}

/// Re-asserts the persisted control plan after a scheduler restart (Issue #147,
/// US-7). Called **after** [`reassert_live`], fail-open: an unreachable engine is
/// only a warning, never a boot failure. Idempotent.
pub async fn reconcile(pool: &SqlitePool) -> ApiResult<()> {
    if let Some(preset) = get_setting(pool, PRESET_KEY).await? {
        if is_valid_preset(&preset) {
            push_best_effort(&crate::liquidsoap::layer_preset(&preset)).await;
        } else {
            tracing::warn!(preset, "control.preset is unknown, skipping engine push");
        }
    }
    if let Some(raw) = get_setting(pool, FORCED_LAYER_KEY).await? {
        let command = match SourceLayer::parse("control.forced_layer", &raw) {
            Ok(layer) => crate::liquidsoap::layer_force(layer.as_str()),
            Err(_) => crate::liquidsoap::layer_force("none"),
        };
        push_best_effort(&command).await;
    }
    if get_setting(pool, PAUSED_KEY).await?.is_some() {
        tracing::info!("control plan pause persisted (advisory, no engine command)");
    }
    Ok(())
}
