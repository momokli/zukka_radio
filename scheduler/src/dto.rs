//! Request/response DTOs and the string enums from consolidated-architecture
//! §3.4. All enums are `snake_case` in JSON, DB and Rust.

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

/// Serde helper: distinguishes "absent" (`None`) from JSON `null` (`Some(None)`).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
