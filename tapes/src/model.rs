//! Public JSON model (`docs/tapes.md` §3.3/§3.4).
//!
//! `status`, `visible`, `storage_key`/`object_key` and `etag` are **never**
//! serialized (`#[serde(skip)]` style omission — they are simply absent).

use serde::Serialize;

/// The minimal caster projection embedded in a tape.
#[derive(Debug, Clone, Serialize)]
pub struct CasterRefDto {
    /// `casters.id` (UUIDv7).
    pub id: String,
    /// `casters.display_name`.
    pub name: String,
    /// Always `null` — the schema has no `accent` column.
    pub accent: Option<String>,
}

/// The minimal show projection embedded in a tape.
#[derive(Debug, Clone, Serialize)]
pub struct ShowRefDto {
    pub id: String,
    pub name: String,
}

/// Peak metadata (no bytes) for a tape that has a waveform row.
#[derive(Debug, Clone, Serialize)]
pub struct PeaksDto {
    pub scheme: i64,
    pub ms_per_bar: i64,
    pub bars: i64,
}

/// The full public tape object (`docs/tapes.md` §3.3).
#[derive(Debug, Clone, Serialize)]
pub struct TapeDto {
    pub slug: String,
    pub caster: CasterRefDto,
    pub show: Option<ShowRefDto>,
    /// Raw title (`null` when never set); see `display_title` for the fallback.
    pub title: Option<String>,
    pub display_title: String,
    pub description: Option<String>,
    /// RFC3339 UTC (`…Z`).
    pub started_at: String,
    /// RFC3339 UTC (`…Z`).
    pub ended_at: String,
    /// Berlin wall time with explicit offset.
    pub start_local: String,
    /// Berlin wall time with explicit offset.
    pub end_local: String,
    pub tz: String,
    pub window_label: String,
    pub duration_ms: Option<i64>,
    pub source: String,
    pub format: Option<String>,
    pub mime: Option<String>,
    pub size_bytes: Option<i64>,
    pub audio_url: String,
    pub download_url: String,
    pub waveform_url: String,
    pub peaks: Option<PeaksDto>,
    pub share_url: String,
    pub published_at: Option<String>,
    pub updated_at: String,
}

/// Feed envelope: `{ tapes, next_cursor, count, server_time }`.
#[derive(Debug, Clone, Serialize)]
pub struct TapeFeedDto {
    pub tapes: Vec<TapeDto>,
    pub next_cursor: Option<String>,
    pub count: usize,
    pub server_time: String,
}

/// One caster aggregate row (`docs/tapes.md` §3.4).
#[derive(Debug, Clone, Serialize)]
pub struct CasterSummaryDto {
    pub id: String,
    pub name: String,
    /// Always `null` — the schema has no `accent` column.
    pub accent: Option<String>,
    pub tape_count: i64,
    pub last_tape_at: Option<String>,
}
