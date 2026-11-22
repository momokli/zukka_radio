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

use std::collections::{BTreeMap, HashMap};

use serde_json::json;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::dto::{
    AutomationOverrideDto, AutomationViewDto, ChannelKindDto, ChannelViewDto, ConnectedCasterDto,
    ControlCommandRequest, ControlCommandResponse, ControlLiveRequest, ControlPauseRequest,
    ControlPlanPutRequest, ControlSkipRequest, ControlStateDto, ControlSwitchRequest, LayerSource,
    LiveStateDto, MasterOutputsDto, MasterViewDto, ObservedNowPlaying, PlanDto, PlanEngineStatus,
    SourceDto, SourceLayer,
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

    // Issue #147: plan projection for the additive channels/master/automation.
    let preset = get_setting(pool, PRESET_KEY)
        .await?
        .filter(|raw| is_valid_preset(raw));
    let forced_since: Option<String> =
        sqlx::query_scalar("SELECT updated_at FROM settings WHERE key = ?")
            .bind(FORCED_LAYER_KEY)
            .fetch_optional(pool)
            .await?;

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
        channels: project_channels(preset.as_deref(), &read_meters().await),
        master: MasterViewDto {
            gain_db: None,
            limiter_active: false,
            outputs: MasterOutputsDto {
                icecast: true,
                hls: true,
                tape: false,
            },
        },
        automation: AutomationViewDto {
            mode: if forced_layer.is_some() {
                "manual".to_string()
            } else {
                "auto".to_string()
            },
            preset,
            next_event_at: None,
            next_source: None,
            override_: forced_layer.map(|layer| AutomationOverrideDto {
                channel: layer.as_str().to_string(),
                since: forced_since,
            }),
        },
    })
}

/// One channel's levels read from `mixer.meters` (§4.5).
struct MeterView {
    rms_l: Option<f64>,
    rms_r: Option<f64>,
    peak: Option<f64>,
    limiter_active: bool,
}

/// Reads `mixer.meters` fail-open: engine gone/unparsable ⇒ empty map (the
/// `state()` response stays `200`, Q1/S1).
async fn read_meters() -> HashMap<String, MeterView> {
    match crate::liquidsoap::read(crate::liquidsoap::MIXER_METERS).await {
        Ok(Some(body)) => parse_meters(&body),
        _ => HashMap::new(),
    }
}

/// Parses the `mixer.meters` JSON array into a by-id map (order-independent).
fn parse_meters(body: &str) -> HashMap<String, MeterView> {
    let mut map = HashMap::new();
    if let Ok(serde_json::Value::Array(rows)) = serde_json::from_str(body.trim()) {
        for row in rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            map.insert(
                id.to_string(),
                MeterView {
                    rms_l: row.get("rms_l").and_then(|v| v.as_f64()),
                    rms_r: row.get("rms_r").and_then(|v| v.as_f64()),
                    peak: row.get("peak").and_then(|v| v.as_f64()),
                    limiter_active: row
                        .get("limiter_active")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                },
            );
        }
    }
    map
}

/// The plan projection of one channel's fader intent (Q1). `(gain_db, mute)`.
fn preset_channel_intent(preset: Option<&str>, id: &str) -> (Option<f64>, bool) {
    let open = match preset {
        Some("live") => "live",
        Some("relay-only") => "relay",
        Some("default") => "rotation",
        // No preset ⇒ the fader intent is unknown (no authoritative read-back).
        _ => return (None, false),
    };
    if id == open {
        (Some(0.0), false)
    } else {
        (None, true)
    }
}

/// Capitalizes the first character of a channel id for the display `label`.
fn channel_label(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Builds the six program channels in §5.5 order (plan projection + meters).
fn project_channels(
    preset: Option<&str>,
    meters: &HashMap<String, MeterView>,
) -> Vec<ChannelViewDto> {
    PROGRAM_CHANNELS
        .iter()
        .map(|id| {
            let meter = meters.get(*id);
            let (gain_db, mute) = preset_channel_intent(preset, id);
            ChannelViewDto {
                id: (*id).to_string(),
                label: channel_label(id),
                kind: ChannelKindDto::from_id(id),
                source: None,
                connected: false,
                gain_db,
                trim_db: 0.0,
                mute,
                solo: false,
                exclusive: false,
                rms_l: meter.and_then(|m| m.rms_l),
                rms_r: meter.and_then(|m| m.rms_r),
                peak: meter.and_then(|m| m.peak),
                limiter_active: meter.map(|m| m.limiter_active).unwrap_or(false),
                metadata: None,
            }
        })
        .collect()
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
            // `default` is the documented start distribution, i.e. the reset
            // target: without this push the engine would keep the last preset's
            // gains (e.g. `live` open) even though the plan has no preset.
            engine.preset = Some(
                push_outcome(&crate::liquidsoap::layer_preset("default"))
                    .await
                    .as_str()
                    .to_string(),
            );
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
    match get_setting(pool, PRESET_KEY).await? {
        Some(preset) if is_valid_preset(&preset) => {
            push_best_effort(&crate::liquidsoap::layer_preset(&preset)).await;
        }
        Some(preset) => {
            tracing::warn!(preset, "control.preset is unknown, skipping engine push");
        }
        // No preset set ⇒ the engine default is the documented start
        // distribution (`default`), so re-assert it. This closes the reset path:
        // a stale preset cannot survive a scheduler restart.
        None => {
            push_best_effort(&crate::liquidsoap::layer_preset("default")).await;
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

// ---------------------------------------------------------------------------
// Issue #147: HTTP command proxy (US-5)
// ---------------------------------------------------------------------------

/// Commands the HTTP proxy accepts (↔ §6.1.1, without read/`state.get`/`ping`).
const PROXY_COMMANDS: [&str; 14] = [
    "mixer.set",
    "mixer.trim",
    "mixer.mute",
    "mixer.solo",
    "mixer.exclusive",
    "mixer.patch",
    "mixer.master",
    "layer.preset",
    "layer.force",
    "layer.enable",
    "live.set",
    "live.clear",
    "tape.start",
    "tape.stop",
];

/// The six program-channel ids (always considered existing).
const PROGRAM_CHANNELS: [&str; 6] = ["live", "show", "queue", "rotation", "relay", "fun"];

/// True for a two-digit slot suffix (`harbor.slot-NN` / `relay.slot-NN`).
fn is_two_digit(s: &str) -> bool {
    s.len() == 2 && s.chars().all(|c| c.is_ascii_digit())
}

/// #152 channel grammar: the six program channels or a slot id
/// (`harbor.slot-NN` / `relay.slot-NN` / `adhoc-N`). Slot existence is deferred
/// to the engine (Q6).
fn valid_channel_syntax(id: &str) -> bool {
    if PROGRAM_CHANNELS.contains(&id) {
        return true;
    }
    if let Some(rest) = id.strip_prefix("harbor.slot-") {
        return is_two_digit(rest);
    }
    if let Some(rest) = id.strip_prefix("relay.slot-") {
        return is_two_digit(rest);
    }
    if let Some(rest) = id.strip_prefix("adhoc-") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// A valid `mixer.patch` source prefix (§4.1).
fn valid_source(spec: &str) -> bool {
    if let Some(rest) = spec.strip_prefix("file:") {
        return !rest.is_empty();
    }
    if let Some(rest) = spec.strip_prefix("https:") {
        return !rest.is_empty();
    }
    if let Some(rest) = spec.strip_prefix("relay:") {
        return !rest.is_empty();
    }
    if let Some(rest) = spec.strip_prefix("harbor:/live/slot-") {
        return is_two_digit(rest);
    }
    false
}

fn obj_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> ApiResult<&'a str> {
    match obj.get(key).and_then(|v| v.as_str()) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ApiError::invalid_args(key, format!("'{key}' is required"))),
    }
}

fn obj_bool(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> ApiResult<bool> {
    match obj.get(key).and_then(|v| v.as_bool()) {
        Some(value) => Ok(value),
        None => Err(ApiError::invalid_args(
            key,
            format!("'{key}' must be a boolean"),
        )),
    }
}

/// Returns the value of `key` unless it is an explicit JSON `null`, which is
/// treated exactly like an absent field (§4.6: `{"gain_db":null}` ⇒
/// `invalid_args`, not `bad_gain`).
fn present<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    obj.get(key).filter(|value| !value.is_null())
}

/// Reads a required numeric argument: absent **or** explicit `null` is
/// `invalid_args` (a missing argument), a present non-number is `bad_gain`.
fn required_num(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> ApiResult<f64> {
    match present(obj, key) {
        None => Err(ApiError::invalid_args(key, format!("'{key}' is required"))),
        Some(value) => value
            .as_f64()
            .ok_or_else(|| ApiError::bad_gain(key, format!("{key} must be a number"))),
    }
}

fn obj_channel(obj: &serde_json::Map<String, serde_json::Value>) -> ApiResult<&str> {
    let channel = obj_str(obj, "channel")?;
    if !valid_channel_syntax(channel) {
        return Err(ApiError::invalid_args(
            "channel",
            format!("invalid channel '{channel}'"),
        ));
    }
    Ok(channel)
}

/// Formats an `f64` as the shortest decimal the engine's parser accepts.
fn fmt_num(value: f64) -> String {
    let text = format!("{value}");
    if text.is_empty() {
        "0".to_string()
    } else {
        text
    }
}

/// Range-checks a dB gain (Q7: the engine does not enforce `[-60,+12]`).
fn check_gain_db(value: f64, field: &'static str) -> ApiResult<f64> {
    if !(-60.0..=12.0).contains(&value) {
        return Err(ApiError::bad_gain(
            field,
            format!("{field} {value} out of range [-60, 12]"),
        ));
    }
    Ok(value)
}

fn check_linear(value: f64) -> ApiResult<f64> {
    if !(0.0..=1.0).contains(&value) {
        return Err(ApiError::bad_gain(
            "linear",
            format!("linear {value} out of range [0, 1]"),
        ));
    }
    Ok(value)
}

/// Builds the positional engine command line for a validated proxy request.
fn build_command(cmd: &str, args: &serde_json::Value) -> ApiResult<String> {
    if !PROXY_COMMANDS.contains(&cmd) {
        return Err(ApiError::unknown_command(cmd));
    }
    let obj = args
        .as_object()
        .ok_or_else(|| ApiError::invalid_args("args", "args must be an object"))?;
    match cmd {
        "mixer.set" => {
            let channel = obj_channel(obj)?;
            match (present(obj, "gain_db"), present(obj, "linear")) {
                (Some(_), Some(_)) => Err(ApiError::invalid_args(
                    "gain_db",
                    "gain_db and linear are mutually exclusive",
                )),
                (Some(value), None) => {
                    let gain = value
                        .as_f64()
                        .ok_or_else(|| ApiError::bad_gain("gain_db", "gain_db must be a number"))?;
                    Ok(crate::liquidsoap::mixer_set(
                        channel,
                        &fmt_num(check_gain_db(gain, "gain_db")?),
                    ))
                }
                (None, Some(value)) => {
                    let linear = value
                        .as_f64()
                        .ok_or_else(|| ApiError::bad_gain("linear", "linear must be a number"))?;
                    Ok(crate::liquidsoap::mixer_set(
                        channel,
                        &fmt_num(check_linear(linear)?),
                    ))
                }
                (None, None) => Err(ApiError::invalid_args(
                    "gain_db",
                    "mixer.set requires exactly one of gain_db or linear",
                )),
            }
        }
        "mixer.trim" => {
            let channel = obj_channel(obj)?;
            let value = required_num(obj, "gain_db")?;
            Ok(crate::liquidsoap::mixer_trim(
                channel,
                &fmt_num(check_gain_db(value, "gain_db")?),
            ))
        }
        "mixer.mute" => {
            let channel = obj_channel(obj)?;
            Ok(crate::liquidsoap::mixer_mute(
                channel,
                obj_bool(obj, "mute")?,
            ))
        }
        "mixer.solo" => {
            let channel = obj_channel(obj)?;
            Ok(crate::liquidsoap::mixer_solo(
                channel,
                obj_bool(obj, "solo")?,
            ))
        }
        "mixer.exclusive" => {
            let channel = obj_channel(obj)?;
            Ok(crate::liquidsoap::mixer_exclusive(channel))
        }
        "mixer.patch" => {
            let channel = obj_channel(obj)?;
            let source = obj_str(obj, "source")?;
            if !valid_source(source) {
                return Err(ApiError::bad_source(format!(
                    "invalid source prefix in '{source}'"
                )));
            }
            Ok(crate::liquidsoap::mixer_patch(channel, source))
        }
        "mixer.master" => {
            let value = required_num(obj, "gain_db")?;
            Ok(crate::liquidsoap::mixer_master(&fmt_num(check_gain_db(
                value, "gain_db",
            )?)))
        }
        "layer.preset" => {
            let preset = obj_str(obj, "preset")?;
            if !is_valid_preset(preset) {
                return Err(ApiError::invalid_args(
                    "preset",
                    format!("unknown preset '{preset}'"),
                ));
            }
            Ok(crate::liquidsoap::layer_preset(preset))
        }
        "layer.force" => {
            let layer = obj_str(obj, "layer")?;
            if layer != "none" && !PROGRAM_CHANNELS.contains(&layer) {
                return Err(ApiError::invalid_args(
                    "layer",
                    format!("unknown layer '{layer}'"),
                ));
            }
            Ok(crate::liquidsoap::layer_force(layer))
        }
        "layer.enable" => {
            let layer = obj_str(obj, "layer")?;
            if !PROGRAM_CHANNELS.contains(&layer) {
                return Err(ApiError::invalid_args(
                    "layer",
                    format!("unknown layer '{layer}'"),
                ));
            }
            Ok(crate::liquidsoap::layer_enable(
                layer,
                obj_bool(obj, "enabled")?,
            ))
        }
        "live.set" => Ok(crate::liquidsoap::live_set(obj_str(obj, "mount")?)),
        "live.clear" => Ok(crate::liquidsoap::LIVE_CLEAR.to_string()),
        "tape.start" => Ok(crate::liquidsoap::tape_start(obj_str(obj, "tape_id")?)),
        "tape.stop" => Ok(crate::liquidsoap::TAPE_STOP.to_string()),
        other => Err(ApiError::unknown_command(other)),
    }
}

/// Maps a parsed engine error to the §6.4 contract error (status + code).
fn engine_error_to_api(code: crate::liquidsoap::EngineErrorCode, body: &str) -> ApiError {
    let message = body.trim().to_string();
    match code {
        crate::liquidsoap::EngineErrorCode::ChannelNotFound => {
            ApiError::channel_not_found(&message)
        }
        crate::liquidsoap::EngineErrorCode::BadGain => ApiError::bad_gain("gain_db", message),
        crate::liquidsoap::EngineErrorCode::BadSource => ApiError::bad_source(message),
        crate::liquidsoap::EngineErrorCode::InvalidArgs => ApiError::invalid_args("args", message),
        crate::liquidsoap::EngineErrorCode::CommandFailed => ApiError::command_failed(message),
    }
}

/// Bounds the raw engine body echoed back to the client.
fn bound_raw(body: &str) -> String {
    const MAX: usize = 512;
    let trimmed = body.trim();
    if trimmed.len() <= MAX {
        trimmed.to_string()
    } else {
        trimmed.chars().take(MAX).collect()
    }
}

/// Parses the engine body into the WS-analog `result` object.
fn parse_result(body: &str) -> serde_json::Value {
    let trimmed = body.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return value;
        }
    }
    json!({ "applied": true })
}

/// Bounds the audited `args` to 1024 characters (serialized form) so an
/// oversized request cannot bloat `event_log.details`.
fn bound_args(args: &serde_json::Value) -> String {
    const MAX: usize = 1024;
    let text = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    if text.chars().count() <= MAX {
        text
    } else {
        text.chars().take(MAX).collect()
    }
}

/// Appends the proxy audit row (exactly one per call, success **and** failure).
async fn audit_command(
    pool: &SqlitePool,
    cmd: &str,
    args: &serde_json::Value,
    outcome: &str,
    engine: &str,
) {
    if let Err(err) = log_control_event(
        pool,
        "control_action",
        None,
        json!({
            "action": "command",
            "cmd": cmd,
            "args": bound_args(args),
            "outcome": outcome,
            "engine": engine,
        }),
    )
    .await
    {
        tracing::warn!("failed to audit control command: {err}");
    }
}

/// `POST /control/command` — the engine command proxy (Issue #147, US-5).
///
/// Validates the command against §4/§6.1.1 **before** the push, maps engine and
/// transport failures to the §6.4 contract codes and audits every call.
pub async fn command(
    pool: &SqlitePool,
    req: &ControlCommandRequest,
) -> ApiResult<ControlCommandResponse> {
    let cmd = req.cmd.trim().to_string();
    let args = req.args.clone().unwrap_or_else(|| json!({}));

    // Fail fast: whitelist + per-command validation, before touching the engine.
    let command_line = match build_command(&cmd, &args) {
        Ok(line) => line,
        Err(err) => {
            let outcome = contract_code(&err);
            audit_command(pool, &cmd, &args, &outcome, "rejected").await;
            return Err(err);
        }
    };

    match crate::liquidsoap::push(&command_line).await {
        // No endpoint configured or transport failure ⇒ 503 (Q3/C11).
        Ok(None) => {
            audit_command(pool, &cmd, &args, "engine_unavailable", "unavailable").await;
            Err(ApiError::engine_unavailable(
                "liquidsoap control endpoint is not configured",
            ))
        }
        Err(err) => {
            audit_command(pool, &cmd, &args, "engine_unavailable", "unavailable").await;
            Err(ApiError::engine_unavailable(err))
        }
        Ok(Some(body)) => match crate::liquidsoap::parse_engine_error(&body) {
            Some(code) => {
                audit_command(pool, &cmd, &args, code.as_str(), "failed").await;
                Err(engine_error_to_api(code, &body))
            }
            None => {
                audit_command(pool, &cmd, &args, "ok", "applied").await;
                Ok(ControlCommandResponse {
                    cmd,
                    result: parse_result(&body),
                    raw: bound_raw(&body),
                })
            }
        },
    }
}

/// The contract `code` of an [`ApiError`] (for the audit `outcome` field).
fn contract_code(err: &ApiError) -> String {
    match err {
        ApiError::Contract { code, .. } => (*code).to_string(),
        ApiError::Validation { .. } => "invalid_args".to_string(),
        _ => "command_failed".to_string(),
    }
}
