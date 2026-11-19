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

/// Serde helper: distinguishes "absent" (`None`) from JSON `null` (`Some(None)`).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
