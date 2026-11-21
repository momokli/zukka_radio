//! Request/response DTOs and the string enums from consolidated-architecture
//! §3.4. All enums are `snake_case` in JSON, DB and Rust.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::rrule::RecurrenceRule;

/// `queues.kind` — `background` | `slot` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum QueueKind {
    Background,
    Slot,
}

impl QueueKind {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "background" => Ok(QueueKind::Background),
            "slot" => Ok(QueueKind::Slot),
            other => Err(ApiError::validation(
                field,
                format!("unknown kind '{other}', expected 'background' or 'slot'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QueueKind::Background => "background",
            QueueKind::Slot => "slot",
        }
    }
}

/// `queue_items.kind` — `track` | `tape` | `link` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum QueueItemKind {
    Track,
    Tape,
    Link,
}

impl QueueItemKind {
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "track" => Ok(QueueItemKind::Track),
            "tape" => Ok(QueueItemKind::Tape),
            "link" => Ok(QueueItemKind::Link),
            other => Err(ApiError::validation(
                field,
                format!("unknown kind '{other}', expected 'track', 'tape' or 'link'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QueueItemKind::Track => "track",
            QueueItemKind::Tape => "tape",
            QueueItemKind::Link => "link",
        }
    }
}

/// `queue_items.state` — `pending` | `playing` | `played` | `skipped` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum QueueItemState {
    Pending,
    Playing,
    Played,
    Skipped,
}

impl QueueItemState {
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "pending" => Ok(QueueItemState::Pending),
            "playing" => Ok(QueueItemState::Playing),
            "played" => Ok(QueueItemState::Played),
            "skipped" => Ok(QueueItemState::Skipped),
            other => Err(ApiError::validation(
                field,
                format!(
                    "unknown state '{other}', expected 'pending', 'playing', 'played' or 'skipped'"
                ),
            )),
        }
    }
}

/// `queue_items.link_provider` — `youtube` | `spotify` | `other` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum LinkProvider {
    Youtube,
    Spotify,
    Other,
}

impl LinkProvider {
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "youtube" => Ok(LinkProvider::Youtube),
            "spotify" => Ok(LinkProvider::Spotify),
            "other" => Ok(LinkProvider::Other),
            other => Err(ApiError::validation(
                field,
                format!(
                    "unknown link_provider '{other}', expected 'youtube', 'spotify' or 'other'"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LinkProvider::Youtube => "youtube",
            LinkProvider::Spotify => "spotify",
            LinkProvider::Other => "other",
        }
    }
}

/// `queue_items.link_state` — `pending` | `ready` | `failed` | `unplayable` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum LinkState {
    Pending,
    Ready,
    Failed,
    Unplayable,
}

/// Full queue representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDto {
    pub id: String,
    pub caster_id: String,
    pub name: String,
    pub description: String,
    pub kind: QueueKind,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /queues`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateQueueRequest {
    pub caster_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Body of `PATCH /queues/{id}` — `version` is mandatory (optimistic lock).
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateQueueRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub version: Option<i64>,
}

/// Full queue item representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItemDto {
    pub id: String,
    pub queue_id: String,
    pub position: i64,
    pub kind: QueueItemKind,
    pub track_id: Option<String>,
    pub tape_id: Option<String>,
    pub link_url: Option<String>,
    pub link_provider: Option<LinkProvider>,
    pub link_state: Option<LinkState>,
    pub title_override: Option<String>,
    pub state: QueueItemState,
    pub played_at: Option<String>,
    pub created_at: String,
}

/// Body of `POST /queues/{id}/items`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateItemRequest {
    pub kind: String,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub tape_id: Option<String>,
    #[serde(default)]
    pub link_url: Option<String>,
    #[serde(default)]
    pub link_provider: Option<String>,
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
}

/// Body of `PATCH /queues/{id}/items/{item_id}` — content fields are immutable.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateItemRequest {
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
}

/// Body of `POST /queues/{id}/reorder` — `version` is mandatory.
#[derive(Debug, Clone, Deserialize)]
pub struct ReorderRequest {
    pub item_ids: Vec<String>,
    #[serde(default)]
    pub version: Option<i64>,
}

// ---------------------------------------------------------------------------
// Issue #11: shows, broadcasts, schedule
// ---------------------------------------------------------------------------

/// `broadcasts.kind` — `live` | `recording` | `playlist` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum BroadcastKind {
    Live,
    Recording,
    Playlist,
}

impl BroadcastKind {
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "live" => Ok(BroadcastKind::Live),
            "recording" => Ok(BroadcastKind::Recording),
            "playlist" => Ok(BroadcastKind::Playlist),
            other => Err(ApiError::validation(
                field,
                format!("unknown kind '{other}', expected 'live', 'recording' or 'playlist'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BroadcastKind::Live => "live",
            BroadcastKind::Recording => "recording",
            BroadcastKind::Playlist => "playlist",
        }
    }
}

/// `broadcasts.status` — `scheduled` | `live` | `ended` | `cancelled` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum BroadcastStatus {
    Scheduled,
    Live,
    Ended,
    Cancelled,
}

impl BroadcastStatus {
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "scheduled" => Ok(BroadcastStatus::Scheduled),
            "live" => Ok(BroadcastStatus::Live),
            "ended" => Ok(BroadcastStatus::Ended),
            "cancelled" => Ok(BroadcastStatus::Cancelled),
            other => Err(ApiError::validation(
                field,
                format!(
                    "unknown status '{other}', expected 'scheduled', 'live', 'ended' or 'cancelled'"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BroadcastStatus::Scheduled => "scheduled",
            BroadcastStatus::Live => "live",
            BroadcastStatus::Ended => "ended",
            BroadcastStatus::Cancelled => "cancelled",
        }
    }
}

/// Full show representation (`shows` row + parsed recurrence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowDto {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub artwork_url: Option<String>,
    pub links: Vec<serde_json::Value>,
    pub recurrence: Option<RecurrenceRule>,
    pub caster_id: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /shows`.
///
/// `recurrence` is a raw rule string: the handler parses it so an invalid
/// rule yields `422 validation_error` (not a generic `400` body error).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateShowRequest {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub links: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub recurrence: Option<String>,
    #[serde(default)]
    pub caster_id: Option<String>,
    pub created_by: String,
}

/// Body of `PATCH /shows/{id}` — COALESCE patch, no `version` (schema).
///
/// Nullable columns use `Option<Option<_>>`: field absent = keep, JSON `null`
/// = clear, value = replace.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateShowRequest {
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub artwork_url: Option<Option<String>>,
    #[serde(default)]
    pub links: Option<Vec<serde_json::Value>>,
    #[serde(default, deserialize_with = "double_option")]
    pub recurrence: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub caster_id: Option<Option<String>>,
}

/// A soft scheduling conflict (§4.2/§4.3). Always serialized, possibly empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarningDto {
    pub code: String,
    pub message: String,
    pub broadcast_id: Option<String>,
    pub show_id: Option<String>,
    pub caster_id: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub severity: String,
}

/// Full broadcast representation (`broadcasts` row + API warnings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastDto {
    pub id: String,
    pub show_id: Option<String>,
    pub caster_id: String,
    pub kind: BroadcastKind,
    pub title: String,
    pub description: String,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub status: BroadcastStatus,
    pub tape_id: Option<String>,
    pub queue_id: Option<String>,
    pub version: i64,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub warnings: Vec<WarningDto>,
}

/// Body of `POST /broadcasts`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateBroadcastRequest {
    pub caster_id: String,
    #[serde(default)]
    pub show_id: Option<String>,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub starts_at: String,
    #[serde(default)]
    pub ends_at: Option<String>,
    #[serde(default)]
    pub tape_id: Option<String>,
    #[serde(default)]
    pub queue_id: Option<String>,
    /// Defaults to `caster_id` until auth supplies `X-Caster-Id` (O-2).
    #[serde(default)]
    pub created_by: Option<String>,
}

/// Body of `PATCH /broadcasts/{id}` — `version` is mandatory (optimistic lock).
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBroadcastRequest {
    #[serde(default, deserialize_with = "double_option")]
    pub show_id: Option<Option<String>>,
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub starts_at: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub ends_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub tape_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub queue_id: Option<Option<String>>,
    #[serde(default)]
    pub version: Option<i64>,
}

/// One merged schedule entry: persisted broadcast or computed occurrence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntryDto {
    /// Broadcast id; `null` for an occurrence that is not persisted yet.
    pub id: Option<String>,
    pub show_id: Option<String>,
    pub caster_id: String,
    pub kind: BroadcastKind,
    pub title: String,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub status: BroadcastStatus,
    pub persisted: bool,
    #[serde(default)]
    pub warnings: Vec<WarningDto>,
}

/// Result of `POST /schedule/expand`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandResultDto {
    pub from: String,
    pub to: String,
    pub created: i64,
    pub skipped: i64,
    #[serde(default)]
    pub warnings: Vec<WarningDto>,
}

// ---------------------------------------------------------------------------
// Issue #12: sources / priority layers
// ---------------------------------------------------------------------------

/// Fixed set of priority layers (§2.4/§3.2, C77). Wire form: lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLayer {
    Live,
    Show,
    Queue,
    Rotation,
    Relay,
    Fun,
}

impl SourceLayer {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "live" => Ok(SourceLayer::Live),
            "show" => Ok(SourceLayer::Show),
            "queue" => Ok(SourceLayer::Queue),
            "rotation" => Ok(SourceLayer::Rotation),
            "relay" => Ok(SourceLayer::Relay),
            "fun" => Ok(SourceLayer::Fun),
            other => Err(ApiError::validation(
                field,
                format!(
                    "unknown layer '{other}', expected one of 'live', 'show', 'queue', \
                     'rotation', 'relay' or 'fun'"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SourceLayer::Live => "live",
            SourceLayer::Show => "show",
            SourceLayer::Queue => "queue",
            SourceLayer::Rotation => "rotation",
            SourceLayer::Relay => "relay",
            SourceLayer::Fun => "fun",
        }
    }
}

/// Full source representation. `name`/`kind` are read-only (C79), `priority`
/// is immutable (C78).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceDto {
    pub id: String,
    /// Layer name — `live|show|queue|rotation|relay|fun`.
    pub name: SourceLayer,
    /// Mirrors `name` in the seed; read-only.
    pub kind: SourceLayer,
    /// `1..=100`, immutable (C78).
    pub priority: i64,
    /// `INTEGER 0/1` → bool.
    pub enabled: bool,
    /// `TEXT` json → Value.
    pub config: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `PATCH /sources/{id}` — only `enabled` and/or `config`.
///
/// Unknown fields (`priority`, `name`, `kind`, `id`, …) are rejected with
/// `400 bad_request` via `deny_unknown_fields` (C77/C78), never silently
/// ignored. `config` uses `double_option`: absent = keep, JSON `null` = `422`,
/// object = replace.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSourceRequest {
    // Deserialized as a raw value so a non-boolean (`0`, `1`, `"true"`) is a
    // field-level `422 validation_error` (validated in the store), not the
    // extractor's generic `400`. An unknown field still yields `400` via
    // `deny_unknown_fields`.
    #[serde(default)]
    pub enabled: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "double_option")]
    pub config: Option<Option<serde_json::Value>>,
}

// ---------------------------------------------------------------------------
// Issue #13: playback API
// ---------------------------------------------------------------------------

/// Playable media kind for [`PlaybackItemDto`] — `track` | `tape` (§2.3).
///
/// Deliberately separate from [`QueueItemKind`]: a `link` is never directly
/// playable (its Music-API URL only exists after resolution, O7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackKind {
    Track,
    Tape,
}

impl PlaybackKind {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "track" => Ok(PlaybackKind::Track),
            "tape" => Ok(PlaybackKind::Tape),
            other => Err(ApiError::validation(
                field,
                format!("unknown kind '{other}', expected 'track' or 'tape'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackKind::Track => "track",
            PlaybackKind::Tape => "tape",
        }
    }
}

/// Requested playback layer — only `show` | `queue` are resolvable (§2.2).
///
/// Any other value (`live`, `rotation`, `relay`, `fun`) becomes
/// `422 validation_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackLayer {
    Show,
    Queue,
}

impl PlaybackLayer {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "show" => Ok(PlaybackLayer::Show),
            "queue" => Ok(PlaybackLayer::Queue),
            other => Err(ApiError::validation(
                field,
                format!("unknown layer '{other}', expected 'show' or 'queue'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackLayer::Show => "show",
            PlaybackLayer::Queue => "queue",
        }
    }
}

/// One resolvable playback item (§3.4). `uri` is a Music-API URL (C46).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackItemDto {
    /// `null` when the item does not come from a queue (e.g. `show` tape).
    pub queue_item_id: Option<String>,
    pub kind: PlaybackKind,
    pub track_id: Option<String>,
    pub tape_id: Option<String>,
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: i64,
    pub uri: String,
    /// Cue points arrive with the link-/cue issue (#13: always `null`).
    pub cue_in_ms: Option<i64>,
    pub cue_out_ms: Option<i64>,
}

/// Body of `POST /playback/started` (§3.5; `source_id` → `layer` per C79/C §4.2).
///
/// Only `queue_item_id` is mandatory; the remaining fields are informational
/// (logging).
#[derive(Debug, Clone, Deserialize)]
pub struct PlaybackStartedRequest {
    #[serde(default)]
    pub queue_item_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub tape_id: Option<String>,
    #[serde(default)]
    pub layer: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
}

/// Body of `POST /playback/ended`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaybackEndedRequest {
    #[serde(default)]
    pub queue_item_id: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
}

/// Body of `POST /playback/error`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaybackErrorRequest {
    #[serde(default)]
    pub queue_item_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub occurred_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #107: public-safe now-playing (scheduler `/now`)
// ---------------------------------------------------------------------------

/// The show of the running broadcast, public-safe (§4.2, Issue #107).
///
/// Only `id` + `name` — never an email, artwork or any internal field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NowShowDto {
    pub id: String,
    pub name: String,
}

/// The caster of the running broadcast, public-safe (§4.2, Issue #107).
///
/// `name` is `casters.display_name`; no username, email or stream hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NowCasterDto {
    pub id: String,
    pub name: String,
}

/// Public-safe now-playing snapshot (`GET /now`, §4.2, Issue #107).
///
/// Deliberately a *projection*, not the raw control state: no `uri`, no
/// `music_api_id`, no stream/credential hashes, no email. `layer`/`source`
/// carry the same active layer (§2.4) — `source` is the plain wire duplicate
/// the tapes projection consumes. Every optional field is `null` when nothing
/// is on air.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NowPlayingDto {
    /// Active output layer (K3 approximation), or `null` when none is active.
    pub layer: Option<SourceLayer>,
    /// Same active layer as `layer` (wire duplicate for consumers).
    pub source: Option<SourceLayer>,
    /// Title of the running item, or `null`.
    pub title: Option<String>,
    /// Artist of the running item, or `null`.
    pub artist: Option<String>,
    /// Show of the running broadcast, or `null`.
    pub show: Option<NowShowDto>,
    /// Caster of the running broadcast, or `null`.
    pub caster: Option<NowCasterDto>,
    /// Start of the running item (`queue_items.played_at`, fallback
    /// `broadcasts.starts_at`), ISO-8601 UTC, or `null`.
    pub started_at: Option<String>,
    /// `true` when the live layer is active (§2.5).
    pub live: bool,
}

// ---------------------------------------------------------------------------
// Issue #15: control API
// ---------------------------------------------------------------------------

/// Body of `POST /control/live`.
///
/// `caster_id` present (non-null) sets the live override; absent or `null`
/// clears it. `reason` is mandatory (validated in the store, `422 field=reason`).
#[derive(Debug, Clone, Deserialize)]
pub struct ControlLiveRequest {
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Body of `POST /control/switch` — force one of the six priority layers.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlSwitchRequest {
    #[serde(default)]
    pub layer: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Body of `POST /control/skip` — skip the currently playing queue item.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlSkipRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Body of `POST /control/pause`.
///
/// `paused` is deserialized as a raw value so a non-boolean is a field-level
/// `422 validation_error` (`details.field=paused`), not the extractor's generic
/// `400` (Vorbild [`UpdateSourceRequest::enabled`]).
#[derive(Debug, Clone, Deserialize)]
pub struct ControlPauseRequest {
    #[serde(default)]
    pub paused: Option<serde_json::Value>,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #22: control-panel state (read-only composition)
// ---------------------------------------------------------------------------

/// The live-layer slice of [`ControlStateDto`] (Issue #22, US-1).
///
/// `connected` is derived from `event_log` (K4) and is usually empty until
/// `POST /live/events` (#32) starts writing live events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveStateDto {
    /// `settings['live.override_caster_id']`, or `null`.
    pub override_caster_id: Option<String>,
    /// Override, else the running live broadcast's caster, else `null`.
    pub active_caster_id: Option<String>,
    /// Casters observed connected, derived from `event_log` (K4), sorted.
    pub connected: Vec<ConnectedCasterDto>,
}

/// One connected caster, derived from `event_log` (K4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedCasterDto {
    pub caster_id: String,
    /// `casters.harbor_mount` of the caster, when known.
    pub mount: Option<String>,
    /// `occurred_at` of the latest `live_connected` event (ISO-8601 UTC).
    pub since: String,
}

/// Full read-only control-panel state (`GET /control/state`, Issue #22, US-1).
///
/// Composed entirely from existing tables (`sources`, `settings`,
/// `broadcasts`, `queue_items`, `tracks`, `event_log`) — no writes, no
/// migration. `derived` marks `active_layer` as the scheduler's deterministic
/// approximation of the output layer (K2/K3) until #32 provides real status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlStateDto {
    /// All six layers (`priority DESC, name ASC`) incl. `enabled`.
    pub sources: Vec<SourceDto>,
    /// `settings['control.paused'] == "true"` (absent ⇒ `false`).
    pub paused: bool,
    /// `settings['control.forced_layer']` as a layer, or `null`.
    pub forced_layer: Option<SourceLayer>,
    /// Effective active output layer = `observed_layer.or(derived_layer)`.
    pub active_layer: Option<SourceLayer>,
    /// Always `true` in #22 — `active_layer` remains an approximation (K2).
    pub derived: bool,
    pub live: LiveStateDto,
    /// `store::playback::current`, or `null` (DB path, unchanged).
    pub now_playing: Option<PlaybackItemDto>,
    /// Layer observed from Icecast metadata, or `null` (Issue #140).
    #[serde(default)]
    pub observed_layer: Option<SourceLayer>,
    /// The K3 derivation from the scheduler DB (Issue #140).
    #[serde(default)]
    pub derived_layer: Option<SourceLayer>,
    /// Where the effective `active_layer` came from, or `null` (Issue #140).
    #[serde(default)]
    pub active_layer_source: Option<LayerSource>,
    /// `true` when `active_layer` is the observed (Icecast-derived) one.
    #[serde(default)]
    pub observed: bool,
    /// Machine-readable observation reason (`silent`, `icecast_unreachable`,
    /// `live`, `db_match`, `rotation_match`, `external`, `relay_setting`,
    /// `unknown`), or `null` (Issue #140).
    #[serde(default)]
    pub observed_reason: Option<String>,
    /// Icecast reports an audible source (false when silent/unreachable).
    #[serde(default)]
    pub on_air: bool,
    /// ICY title/artist from Icecast (relay/rotation), or `null`.
    #[serde(default)]
    pub observed_now_playing: Option<ObservedNowPlaying>,
}

/// Origin of the effective control-state layer (Issue #140).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerSource {
    /// K3 approximation from the scheduler DB.
    Derived,
    /// Derived from the Icecast status document.
    Observed,
}

impl LayerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LayerSource::Derived => "derived",
            LayerSource::Observed => "observed",
        }
    }
}

/// ICY now-playing as observed on the Icecast output (Issue #140).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedNowPlaying {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #16: auth (identity authority)
// ---------------------------------------------------------------------------

/// Full caster representation, joined with the role name and the role's
/// permission map (§6.10.1). `role` is the role *name* (`admin`/`operator`/
/// `caster`); the portal checks permissions, never the role string (D5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasterDto {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub role: String,
    pub is_active: bool,
    pub harbor_mount: Option<String>,
    pub stream_username: Option<String>,
    pub permissions: BTreeMap<String, bool>,
}

/// Body of `POST /internal/casters` (Issue #101).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCasterRequest {
    pub username: String,
    pub display_name: String,
    pub password: String,
    pub role: String,
    #[serde(default)]
    pub harbor_mount: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub stream_username: Option<String>,
}

/// Response of `POST /internal/casters`: the caster plus the generated stream
/// password, which is transported exactly once (never stored in clear).
#[derive(Debug, Clone, Serialize)]
pub struct CreatedCasterResponse {
    pub caster: CasterDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_password: Option<String>,
}

/// Body of `PATCH /internal/casters/{id}` (Issue #101). Only mutable fields;
/// username and harbor mount are fixed after creation.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCasterRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

/// Body of `POST /internal/auth/verify` (§4.2).
#[derive(Debug, Clone, Deserialize)]
pub struct VerifyRequest {
    pub username: String,
    pub password: String,
}

/// Response of `POST /internal/auth/verify` — `{caster}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResponse {
    pub caster: CasterDto,
}

/// Body of `POST /internal/sessions` (portal login, §4.2 write-through).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionRequest {
    pub caster_id: String,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
}

/// Response of `POST /internal/sessions`. `token` is the raw bearer value and
/// is returned exactly once; the DB holds only `sha256(token)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedSessionDto {
    pub id: String,
    pub token: String,
    pub csrf_token: String,
    pub idle_expires_at: String,
    pub absolute_expires_at: String,
}

/// Response of `GET /internal/sessions/current` — everything the portal needs
/// to build its `AuthContext` without holding authoritative session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionValidationDto {
    pub session_id: String,
    pub caster: CasterDto,
    pub csrf_token: String,
    pub idle_expires_at: String,
    pub absolute_expires_at: String,
}

/// Body of `POST /internal/casters/{id}/password`.
///
/// `keep_session_id` is the caller's current session; it survives the rotation
/// triggered by a password change (all *other* sessions are revoked).
#[derive(Debug, Clone, Deserialize)]
pub struct ChangePasswordRequest {
    pub password: String,
    #[serde(default)]
    pub keep_session_id: Option<String>,
}

/// Body of `POST /internal/harbor/auth` (Issue #33, §4.2).
///
/// Liquidsoap presents the mount it is serving, the caster's stream username
/// and the stream password from the ingest connection.
#[derive(Debug, Clone, Deserialize)]
pub struct HarborAuthRequest {
    pub mount: String,
    pub username: String,
    pub password: String,
}

/// Response of `POST /internal/harbor/auth`. Only returned on success; every
/// failure is the generic `401 unauthorized` envelope (fail-closed, no
/// enumeration).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarborAuthResponse {
    pub caster_id: String,
    pub display_name: String,
    pub mount: String,
}

/// Body of `POST /internal/casters/{id}/stream-password`. An absent `password`
/// asks the scheduler to generate one.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamPasswordRequest {
    #[serde(default)]
    pub password: Option<String>,
}

/// Response of `POST /internal/casters/{id}/stream-password`. `password` is set
/// only when the server generated one, and is shown exactly once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPasswordResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #18: upload jobs (write-through, §4.2)
// ---------------------------------------------------------------------------

/// Body of `POST /internal/upload-jobs` (§4.2).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUploadJobRequest {
    pub caster_id: String,
    pub filename: String,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Body of `PATCH /internal/upload-jobs/{id}`. `state` is mandatory; the
/// optional fields overwrite only when present (`error` is always replaced).
#[derive(Debug, Clone, Deserialize)]
pub struct PatchUploadJobRequest {
    pub state: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Full upload-job representation (`upload_jobs` row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadJobDto {
    pub id: String,
    pub caster_id: Option<String>,
    pub filename: String,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub state: String,
    pub track_id: Option<String>,
    pub error: Option<String>,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Issue #19: tracks (Library)
// ---------------------------------------------------------------------------

/// `tracks.visibility` — `shared` | `private` (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Visibility {
    Shared,
    Private,
}

impl Visibility {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "shared" => Ok(Visibility::Shared),
            "private" => Ok(Visibility::Private),
            other => Err(ApiError::validation(
                field,
                format!("unknown visibility '{other}', expected 'shared' or 'private'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Shared => "shared",
            Visibility::Private => "private",
        }
    }
}

/// Full track representation (`tracks` row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackDto {
    pub id: String,
    pub music_api_id: String,
    pub isrc: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: i64,
    pub storage_key: String,
    pub artwork_url: Option<String>,
    pub visibility: Visibility,
    pub caster_id: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /tracks` — the local projection of a Music-API track.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTrackRequest {
    pub music_api_id: String,
    pub title: String,
    pub storage_key: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Body of `PATCH /tracks/{id}` — COALESCE patch (no `version`).
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTrackRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub artwork_url: Option<Option<String>>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub caster_id: Option<Option<String>>,
}

/// Body of `POST /tracks/import` — a track created from Music-API fields.
#[derive(Debug, Clone, Deserialize)]
pub struct ImportTrackRequest {
    pub music_api_id: String,
    pub title: String,
    pub storage_key: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub caster_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #18: waveforms (D8/C05; write-through to `/internal/waveforms`)
// ---------------------------------------------------------------------------

/// Body of `POST /internal/waveforms` — upsert one waveform row for a track or
/// a tape (`waveforms`, PK `(subject_kind, subject_id)`).
#[derive(Debug, Clone, Deserialize)]
pub struct UpsertWaveformRequest {
    /// `track` | `tape` (`waveforms.subject_kind` CHECK).
    pub subject_kind: String,
    pub subject_id: String,
    /// Format version (`1` = v1).
    pub scheme: i64,
    /// Algorithm tag, e.g. `ffmpeg-minmax-8k/1`.
    pub algorithm: String,
    /// Milliseconds covered by one bar.
    pub ms_per_bar: i64,
    /// Number of bars.
    pub bars: i64,
    /// Raw peaks; `2 * bars` bytes of interleaved 8-bit `(min, max)` pairs.
    pub data: Vec<u8>,
}

/// Full waveform representation (`waveforms` row). `data` is transferred as a
/// JSON array of byte values (small: `2 * bars`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformDto {
    pub subject_kind: String,
    pub subject_id: String,
    pub scheme: i64,
    pub algorithm: String,
    pub ms_per_bar: i64,
    pub bars: i64,
    pub data: Vec<u8>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Issue #25: tape job queue (waveform pipeline, write-through)
// ---------------------------------------------------------------------------

/// Body of `POST /internal/tape-jobs/claim`. All fields optional: `kinds`
/// defaults to `["probe", "peaks"]`, `limit` to `1`, `lease_secs` to `600`.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimTapeJobsRequest {
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub lease_secs: Option<i64>,
}

/// One claimed job, joined with the tape fields the worker needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeJobClaimDto {
    pub id: String,
    pub tape_id: String,
    pub kind: String,
    pub attempts: i64,
    pub slug: String,
    pub storage_key: Option<String>,
    pub source: String,
}

/// Response of `POST /internal/tape-jobs/claim` (empty array when nothing is
/// due).
#[derive(Debug, Clone, Serialize)]
pub struct ClaimTapeJobsResponse {
    pub jobs: Vec<TapeJobClaimDto>,
}

/// Body of `PATCH /internal/tape-jobs/{id}`.
///
/// `state = "done"` on a `probe` job requires `duration_ms`; `state =
/// "pending"` is a retry (backoff is derived from `attempts` + `updated_at`),
/// `state = "failed"` is terminal for the worker.
#[derive(Debug, Clone, Deserialize)]
pub struct PatchTapeJobRequest {
    pub state: String,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Full tape-job representation (`tape_jobs` row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeJobDto {
    pub id: String,
    pub tape_id: String,
    pub kind: String,
    pub state: String,
    pub error: Option<String>,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Issue #34: announcement templates (M5 — Announcements)
// ---------------------------------------------------------------------------

/// `announcement_templates.event` — the seven announcement triggers
/// (`docs/data-model.md` §2.7; schema wins: `fallback_entered`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum AnnouncementEvent {
    LiveOn,
    LiveOff,
    ShowReminder,
    RecordingPublished,
    FallbackEntered,
    Error,
    Manual,
}

impl AnnouncementEvent {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "live_on" => Ok(AnnouncementEvent::LiveOn),
            "live_off" => Ok(AnnouncementEvent::LiveOff),
            "show_reminder" => Ok(AnnouncementEvent::ShowReminder),
            "recording_published" => Ok(AnnouncementEvent::RecordingPublished),
            "fallback_entered" => Ok(AnnouncementEvent::FallbackEntered),
            "error" => Ok(AnnouncementEvent::Error),
            "manual" => Ok(AnnouncementEvent::Manual),
            other => Err(ApiError::validation(
                field,
                format!(
                    "unknown event '{other}', expected one of 'live_on', 'live_off', \
                     'show_reminder', 'recording_published', 'fallback_entered', 'error' \
                     or 'manual'"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AnnouncementEvent::LiveOn => "live_on",
            AnnouncementEvent::LiveOff => "live_off",
            AnnouncementEvent::ShowReminder => "show_reminder",
            AnnouncementEvent::RecordingPublished => "recording_published",
            AnnouncementEvent::FallbackEntered => "fallback_entered",
            AnnouncementEvent::Error => "error",
            AnnouncementEvent::Manual => "manual",
        }
    }
}

/// `announcement_templates.channel` — only `telegram` today (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum AnnouncementChannel {
    Telegram,
}

impl AnnouncementChannel {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "telegram" => Ok(AnnouncementChannel::Telegram),
            other => Err(ApiError::validation(
                field,
                format!("unknown channel '{other}', expected 'telegram'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AnnouncementChannel::Telegram => "telegram",
        }
    }
}

/// Full announcement-template representation (`announcement_templates` row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnouncementTemplateDto {
    pub id: String,
    pub name: String,
    pub event: AnnouncementEvent,
    /// `None` = global default; `Some` = per-show override.
    pub show_id: Option<String>,
    pub channel: AnnouncementChannel,
    pub body: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /announcement-templates`. `channel` defaults to `telegram`,
/// `enabled` to `true`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAnnouncementTemplateRequest {
    pub name: String,
    pub event: String,
    #[serde(default)]
    pub show_id: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    pub body: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Body of `PATCH /announcement-templates/{id}` — COALESCE patch. `show_id`
/// uses `double_option`: absent = keep, JSON `null` = make global.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAnnouncementTemplateRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub show_id: Option<Option<String>>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Body of `POST /announcement-templates/{id}/render` (preview).
#[derive(Debug, Clone, Deserialize)]
pub struct RenderTemplateRequest {
    #[serde(default)]
    pub payload: BTreeMap<String, String>,
}

/// Response of `POST /announcement-templates/{id}/render`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderTemplateResponse {
    pub rendered: String,
    pub unknown_placeholders: Vec<String>,
}

// ---------------------------------------------------------------------------
// Issue #35: announcements (M5 — Events)
// ---------------------------------------------------------------------------

/// `announcements.status` — `pending` | `sent` | `failed` | `cancelled` (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum AnnouncementStatus {
    Pending,
    Sent,
    Failed,
    Cancelled,
}

impl AnnouncementStatus {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "pending" => Ok(AnnouncementStatus::Pending),
            "sent" => Ok(AnnouncementStatus::Sent),
            "failed" => Ok(AnnouncementStatus::Failed),
            "cancelled" => Ok(AnnouncementStatus::Cancelled),
            other => Err(ApiError::validation(
                field,
                format!(
                    "unknown status '{other}', expected 'pending', 'sent', 'failed' or \
                     'cancelled'"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AnnouncementStatus::Pending => "pending",
            AnnouncementStatus::Sent => "sent",
            AnnouncementStatus::Failed => "failed",
            AnnouncementStatus::Cancelled => "cancelled",
        }
    }
}

/// `event_log.event` — the 21 values of the frozen CHECK (Issue #35).
///
/// Mirrors the schema exactly (`fallback`, not `fallback_entered`); only the
/// `/event-log` filter needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum EventLogEvent {
    BroadcastCreated,
    BroadcastUpdated,
    BroadcastCancelled,
    BroadcastStarted,
    BroadcastEnded,
    SourceSwitched,
    LiveConnected,
    LiveDisconnected,
    LiveOverrideSet,
    LiveOverrideCleared,
    Fallback,
    Error,
    DstAdjusted,
    AnnouncementSent,
    AnnouncementFailed,
    CasterCreated,
    CasterUpdated,
    RoleChanged,
    TapePublished,
    TapeUnpublished,
    ControlAction,
}

impl EventLogEvent {
    /// Parses the wire form; unknown values become `422 validation_error`.
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw {
            "broadcast_created" => Ok(EventLogEvent::BroadcastCreated),
            "broadcast_updated" => Ok(EventLogEvent::BroadcastUpdated),
            "broadcast_cancelled" => Ok(EventLogEvent::BroadcastCancelled),
            "broadcast_started" => Ok(EventLogEvent::BroadcastStarted),
            "broadcast_ended" => Ok(EventLogEvent::BroadcastEnded),
            "source_switched" => Ok(EventLogEvent::SourceSwitched),
            "live_connected" => Ok(EventLogEvent::LiveConnected),
            "live_disconnected" => Ok(EventLogEvent::LiveDisconnected),
            "live_override_set" => Ok(EventLogEvent::LiveOverrideSet),
            "live_override_cleared" => Ok(EventLogEvent::LiveOverrideCleared),
            "fallback" => Ok(EventLogEvent::Fallback),
            "error" => Ok(EventLogEvent::Error),
            "dst_adjusted" => Ok(EventLogEvent::DstAdjusted),
            "announcement_sent" => Ok(EventLogEvent::AnnouncementSent),
            "announcement_failed" => Ok(EventLogEvent::AnnouncementFailed),
            "caster_created" => Ok(EventLogEvent::CasterCreated),
            "caster_updated" => Ok(EventLogEvent::CasterUpdated),
            "role_changed" => Ok(EventLogEvent::RoleChanged),
            "tape_published" => Ok(EventLogEvent::TapePublished),
            "tape_unpublished" => Ok(EventLogEvent::TapeUnpublished),
            "control_action" => Ok(EventLogEvent::ControlAction),
            other => Err(ApiError::validation(
                field,
                format!("unknown event '{other}'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EventLogEvent::BroadcastCreated => "broadcast_created",
            EventLogEvent::BroadcastUpdated => "broadcast_updated",
            EventLogEvent::BroadcastCancelled => "broadcast_cancelled",
            EventLogEvent::BroadcastStarted => "broadcast_started",
            EventLogEvent::BroadcastEnded => "broadcast_ended",
            EventLogEvent::SourceSwitched => "source_switched",
            EventLogEvent::LiveConnected => "live_connected",
            EventLogEvent::LiveDisconnected => "live_disconnected",
            EventLogEvent::LiveOverrideSet => "live_override_set",
            EventLogEvent::LiveOverrideCleared => "live_override_cleared",
            EventLogEvent::Fallback => "fallback",
            EventLogEvent::Error => "error",
            EventLogEvent::DstAdjusted => "dst_adjusted",
            EventLogEvent::AnnouncementSent => "announcement_sent",
            EventLogEvent::AnnouncementFailed => "announcement_failed",
            EventLogEvent::CasterCreated => "caster_created",
            EventLogEvent::CasterUpdated => "caster_updated",
            EventLogEvent::RoleChanged => "role_changed",
            EventLogEvent::TapePublished => "tape_published",
            EventLogEvent::TapeUnpublished => "tape_unpublished",
            EventLogEvent::ControlAction => "control_action",
        }
    }
}

/// Full announcement representation (`announcements` row). Timestamps are
/// emitted as Berlin wall time (`fmt_api_time`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnouncementDto {
    pub id: String,
    pub event: AnnouncementEvent,
    pub show_id: Option<String>,
    pub broadcast_id: Option<String>,
    pub caster_id: Option<String>,
    pub template_id: Option<String>,
    pub channel: AnnouncementChannel,
    pub payload: serde_json::Value,
    pub dedupe_key: Option<String>,
    pub scheduled_at: Option<String>,
    pub sent_at: Option<String>,
    pub status: AnnouncementStatus,
    pub attempts: i64,
    pub next_attempt_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /announcements` (explicit creation; `channel` defaults to
/// `telegram`).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAnnouncementRequest {
    pub event: String,
    #[serde(default)]
    pub show_id: Option<String>,
    #[serde(default)]
    pub broadcast_id: Option<String>,
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub scheduled_at: Option<String>,
}

/// Body of `PATCH /announcements/{id}` — COALESCE patch. `status` may only be
/// `cancelled`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAnnouncementRequest {
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub scheduled_at: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Query parameters of `GET /announcements` (all optional).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnnouncementListParams {
    pub status: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub event: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #36: announcement timings
// ---------------------------------------------------------------------------

/// Effective announcement timings in seconds (§7 O11: 900 / 0 / 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncementTimingsDto {
    pub show_reminder_lead_seconds: i64,
    pub live_on_delay_seconds: i64,
    pub live_off_delay_seconds: i64,
}

/// Per-show timings: the effective values plus the explicit overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowAnnouncementTimingsDto {
    pub show_id: String,
    pub effective: AnnouncementTimingsDto,
    /// Only set fields; `null` = inherits the global value.
    pub overrides: AnnouncementTimingsOverridesDto,
}

/// Per-show overrides; `null` per field = inherit the global value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnnouncementTimingsOverridesDto {
    pub show_reminder_lead_seconds: Option<i64>,
    pub live_on_delay_seconds: Option<i64>,
    pub live_off_delay_seconds: Option<i64>,
}

/// `PATCH /announcement-timings` — only the set fields change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAnnouncementTimingsRequest {
    #[serde(default)]
    pub show_reminder_lead_seconds: Option<i64>,
    #[serde(default)]
    pub live_on_delay_seconds: Option<i64>,
    #[serde(default)]
    pub live_off_delay_seconds: Option<i64>,
}

/// `PUT /announcement-timings/shows/{show_id}` —
/// absent = unchanged, `null` = inherit, number = override.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateShowAnnouncementTimingsRequest {
    #[serde(default, deserialize_with = "double_option")]
    pub show_reminder_lead_seconds: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double_option")]
    pub live_on_delay_seconds: Option<Option<i64>>,
    #[serde(default, deserialize_with = "double_option")]
    pub live_off_delay_seconds: Option<Option<i64>>,
}

/// One `event_log` row (`GET /event-log`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogEntryDto {
    pub id: String,
    pub occurred_at: String,
    pub event: EventLogEvent,
    pub actor_caster_id: Option<String>,
    pub broadcast_id: Option<String>,
    pub caster_id: Option<String>,
    pub source_layer: Option<String>,
    pub details: serde_json::Value,
    pub created_at: String,
}

/// Query parameters of `GET /event-log` (all optional).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventLogParams {
    pub from: Option<String>,
    pub to: Option<String>,
    pub event: Option<String>,
    pub broadcast_id: Option<String>,
    pub caster_id: Option<String>,
}

/// `GET /internal/ops/audience` — Deploy-Gate snapshot (#42, §4.2/§2.9).
///
/// `next_broadcast_start == null` means **no future scheduled broadcast**
/// exists; the gate's third clause (`next > now + 15 min`) is then considered
/// satisfied. The wrapper must treat `null` as "no upcoming broadcast" and
/// never compare an empty string (it would abort under `set -e`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudienceDto {
    /// Total listeners across all Icecast sources (0 on poll failure).
    pub listeners: u64,
    /// Running live sources: `kind='live' AND status='live'` + live override.
    pub live_sources: u64,
    /// Earliest future `scheduled` broadcast start, RFC3339 Berlin offset.
    pub next_broadcast_start: Option<String>,
}

/// Serde helper: distinguishes "absent" (`None`) from JSON `null` (`Some(None)`).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
