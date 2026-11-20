//! Persistence layer: runtime `sqlx` queries (no compile-time macros, so CI
//! works without a database).

pub mod announcement_templates;
pub mod auth;
pub mod broadcasts;
pub mod control;
pub mod playback;
pub mod queue_items;
pub mod queues;
pub mod rotation;
pub mod schedule;
pub mod shows;
pub mod sources;
pub mod tape_jobs;
pub mod tracks;
pub mod upload_jobs;
pub mod waveforms;

use chrono::{SecondsFormat, Utc};

/// Current UTC timestamp in the canonical storage format
/// `YYYY-MM-DDTHH:MM:SS.sssZ` (§3.1).
pub(crate) fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
