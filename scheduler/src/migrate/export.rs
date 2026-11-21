//! Legacy export format (Issue #46 "Datenmigration").
//!
//! The migrator never talks to AzuraCast/Funkwhale directly. It consumes a
//! *documented* JSON export (see `docs/data-migration.md`) that a one-off
//! export job produces out of band. The schema below mirrors the legacy fields
//! named in the plan:
//!
//! - Caster: `username`, `display_name`, `email`, `aktiv`
//! - Track: AzuraCast file id, `title`, `artist`, `album`, `duration_ms`,
//!   `storage_key` (path), `isrc`
//! - Tape: recording/broadcast id, `caster` (streamer), `title`,
//!   `started_at`, `ended_at`, `storage_key`, `format` (ogg|mp3), `size_bytes`
//!
//! All timestamps are RFC3339 **UTC** instants. The legacy importer never
//! re-applies the historic hard-coded +2h (CEST) offset — the export is
//! responsible for emitting correct UTC (plan "Legacy-Zeitfalle").

use serde::Deserialize;

/// Top-level legacy export document.
#[derive(Debug, Clone, Deserialize)]
pub struct LegacyExport {
    /// Export schema version; the migrator understands version 1.
    #[serde(default)]
    pub version: u32,
    /// Produced-at instant (informational only).
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub casters: Vec<LegacyCaster>,
    #[serde(default)]
    pub tracks: Vec<LegacyTrack>,
    #[serde(default)]
    pub tapes: Vec<LegacyTape>,
}

impl LegacyExport {
    /// Structural validation that is independent of the target database.
    ///
    /// Returns a human-readable reason on failure (the caller turns this into
    /// a validation error with exit code 1 — never a panic).
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 0 && self.version != 1 {
            return Err(format!(
                "unsupported export version {}: this migrator understands version 1",
                self.version
            ));
        }
        Ok(())
    }
}

/// A legacy caster/streamer account.
#[derive(Debug, Clone, Deserialize)]
pub struct LegacyCaster {
    /// Legacy login name (natural key, compared case-insensitively).
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email: Option<String>,
    /// Legacy "aktiv" flag → `casters.is_active`.
    #[serde(default = "default_true")]
    pub aktiv: bool,
}

/// A legacy library track (AzuraCast projection).
#[derive(Debug, Clone, Deserialize)]
pub struct LegacyTrack {
    /// AzuraCast file id → stringified `tracks.music_api_id`.
    pub file_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: i64,
    /// Legacy path / storage key.
    pub storage_key: String,
    #[serde(default)]
    pub isrc: Option<String>,
    /// Owner caster username; `None`/empty means unowned (`caster_id = NULL`).
    #[serde(default)]
    pub caster: Option<String>,
}

/// A legacy recording or upload.
#[derive(Debug, Clone, Deserialize)]
pub struct LegacyTape {
    /// Recording id; part of the natural key.
    pub legacy_id: String,
    /// Present for live broadcasts → `source = 'live'`; absent → `'upload'`.
    #[serde(default)]
    pub broadcast_id: Option<String>,
    /// Streamer username (required; missing/unresolvable → orphan).
    pub caster: String,
    #[serde(default)]
    pub title: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    #[serde(default)]
    pub storage_key: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub music_api_id: Option<String>,
}

impl LegacyTape {
    /// Natural key used for the deterministic slug: the broadcast id when the
    /// tape came from a live broadcast, otherwise the recording id.
    pub fn natural_key(&self) -> &str {
        match self.broadcast_id.as_deref() {
            Some(bc) if !bc.trim().is_empty() => bc,
            _ => &self.legacy_id,
        }
    }
}

fn default_true() -> bool {
    true
}
