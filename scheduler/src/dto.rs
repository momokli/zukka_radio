//! Request/response DTOs and the string enums from consolidated-architecture
//! §3.4. All enums are `snake_case` in JSON, DB and Rust.

use serde::{Deserialize, Serialize};

use crate::error::ApiError;

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
