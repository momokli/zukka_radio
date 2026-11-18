//! Persistence layer: runtime `sqlx` queries (no compile-time macros, so CI
//! works without a database).

pub mod queue_items;
pub mod queues;

use chrono::{SecondsFormat, Utc};

/// Current UTC timestamp in the canonical storage format
/// `YYYY-MM-DDTHH:MM:SS.sssZ` (§3.1).
pub(crate) fn now_ts() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
