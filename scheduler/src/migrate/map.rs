//! Legacy → target mapping (Issue #46, Stories 2–4).
//!
//! Pure field mapping from [`crate::migrate::export`] records onto the insert
//! shapes of `casters`, `tracks` and `tapes`. Nothing here touches the
//! database; orchestration lives in [`crate::migrate`].

use chrono::{DateTime, SecondsFormat, Utc};

use crate::migrate::export::{LegacyCaster, LegacyTape, LegacyTrack};

/// Seeded role id for `caster` (`0001_init.sql`).
pub const ROLE_CASTER: &str = "0192f0a1-0001-7000-8000-000000000003";

/// Legacy username → preferred display name (plan §Fakten).
pub const DISPLAY_NAMES: &[(&str, &str)] =
    &[("hovio", "ANKD"), ("mctoastus", "duck"), ("momo", "momo")];

/// Argon2id PHC hash of a random 32-byte throwaway secret that is not stored
/// anywhere. A migrated caster therefore cannot log in with any password and
/// must complete a password reset (portal.md §3.6). The value is a constant so
/// re-runs are byte-identical and idempotent.
pub const LOCKED_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$I9ESW765LT7lm5S/4XFdew$FubpdTtJ1EjnBEKtWfmOY+d8QNqypKaLquEvY6CR86o";

/// Target row for `casters`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasterRow {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub role_id: String,
    pub is_active: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Target row for `tracks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRow {
    pub id: String,
    pub music_api_id: String,
    pub isrc: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: i64,
    pub storage_key: String,
    pub visibility: String,
    pub caster_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Target row for `tapes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapeRow {
    pub id: String,
    pub slug: String,
    pub caster_id: String,
    pub title: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: Option<i64>,
    pub source: String,
    pub music_api_id: Option<String>,
    pub storage_key: Option<String>,
    pub format: Option<String>,
    pub size_bytes: Option<i64>,
    pub status: String,
    pub visible: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Formats an instant in the canonical storage shape `…sssZ` (UTC, ms).
pub fn fmt_utc(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Parses a legacy timestamp; only RFC3339 **UTC** instants are accepted (no
/// Berlin/CEST reinterpretation).
pub fn parse_utc(raw: &str) -> Result<DateTime<Utc>, String> {
    let raw = raw.trim();
    let dt = DateTime::parse_from_rfc3339(raw)
        .map_err(|e| format!("invalid RFC3339 timestamp '{raw}': {e}"))?;
    if dt.offset().local_minus_utc() != 0 {
        return Err(format!(
            "timestamp '{raw}' is not UTC (offset {}); the export must emit UTC",
            dt.offset()
        ));
    }
    Ok(dt.with_timezone(&Utc))
}

/// Lowercases and trims a username for NOCASE comparison.
pub fn normalize_username(username: &str) -> String {
    username.trim().to_lowercase()
}

/// Resolves the display name: legacy `DISPLAY_NAMES` mapping wins, else the
/// legacy value, else the username.
pub fn resolve_display_name(username_lower: &str, legacy_display: &str) -> String {
    if let Some((_, mapped)) = DISPLAY_NAMES.iter().find(|(key, _)| *key == username_lower) {
        return (*mapped).to_string();
    }
    let trimmed = legacy_display.trim();
    if trimmed.is_empty() {
        username_lower.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Maps a legacy caster onto a `casters` row.
pub fn map_caster(id: String, legacy: &LegacyCaster, ts: &str) -> CasterRow {
    let username = normalize_username(&legacy.username);
    let display_name = resolve_display_name(&username, &legacy.display_name);
    let email = legacy
        .email
        .as_ref()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .map(|e| e.to_string());
    CasterRow {
        id,
        username,
        display_name,
        email,
        password_hash: LOCKED_PASSWORD_HASH.to_string(),
        role_id: ROLE_CASTER.to_string(),
        is_active: i64::from(legacy.aktiv),
        created_at: ts.to_string(),
        updated_at: ts.to_string(),
    }
}

/// Maps a legacy track onto a `tracks` row. `caster_id` is the resolved owner
/// (or `None` for an unowned track).
pub fn map_track(
    id: String,
    legacy: &LegacyTrack,
    caster_id: Option<String>,
    ts: &str,
) -> Result<TrackRow, String> {
    if legacy.storage_key.trim().is_empty() {
        return Err("storage_key must not be empty".to_string());
    }
    if legacy.duration_ms < 0 {
        return Err(format!(
            "duration_ms must be >= 0, got {}",
            legacy.duration_ms
        ));
    }
    let isrc = legacy
        .isrc
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Ok(TrackRow {
        id,
        music_api_id: legacy.file_id.to_string(),
        isrc,
        title: legacy.title.clone(),
        artist: legacy.artist.clone(),
        album: legacy.album.clone(),
        duration_ms: legacy.duration_ms,
        storage_key: legacy.storage_key.clone(),
        visibility: "shared".to_string(),
        caster_id,
        created_at: ts.to_string(),
        updated_at: ts.to_string(),
    })
}

/// Maps a legacy tape onto a `tapes` row. `caster_id` must be resolved by the
/// caller (missing/unresolvable is an orphan handled upstream).
pub fn map_tape(
    id: String,
    legacy: &LegacyTape,
    caster_id: String,
    slug: String,
    ts: &str,
) -> Result<TapeRow, String> {
    let started = parse_utc(&legacy.started_at)?;
    let ended = parse_utc(&legacy.ended_at)?;
    if ended < started {
        return Err(format!(
            "ended_at ({}) must be >= started_at ({})",
            legacy.ended_at, legacy.started_at
        ));
    }
    if let Some(format) = legacy.format.as_deref() {
        let format = format.trim();
        if !format.is_empty() && !matches!(format, "ogg" | "mp3") {
            return Err(format!("unsupported format '{format}' (expected ogg|mp3)"));
        }
    }
    let duration_ms = (ended - started).num_milliseconds();
    let source = if legacy
        .broadcast_id
        .as_deref()
        .is_some_and(|b| !b.trim().is_empty())
    {
        "live"
    } else {
        "upload"
    };
    let music_api_id = legacy
        .music_api_id
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let storage_key = legacy
        .storage_key
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let format = legacy
        .format
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let title = legacy
        .title
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Ok(TapeRow {
        id,
        slug,
        caster_id,
        title,
        started_at: fmt_utc(started),
        ended_at: fmt_utc(ended),
        duration_ms: Some(duration_ms),
        source: source.to_string(),
        music_api_id,
        storage_key,
        format,
        size_bytes: legacy.size_bytes,
        status: "ready".to_string(),
        visible: 0,
        created_at: ts.to_string(),
        updated_at: ts.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_mapping_applies() {
        assert_eq!(resolve_display_name("hovio", "hovio"), "ANKD");
        assert_eq!(resolve_display_name("mctoastus", ""), "duck");
        assert_eq!(resolve_display_name("momo", "momo"), "momo");
        assert_eq!(resolve_display_name("other", "Other Name"), "Other Name");
        assert_eq!(resolve_display_name("other", ""), "other");
    }

    #[test]
    fn locked_hash_is_argon2id() {
        assert!(LOCKED_PASSWORD_HASH.starts_with("$argon2id$"));
    }

    #[test]
    fn parse_utc_rejects_non_utc() {
        assert!(parse_utc("2024-01-01T19:00:00.000Z").is_ok());
        assert!(parse_utc("2024-01-01T19:00:00.000+02:00").is_err());
        assert!(parse_utc("nonsense").is_err());
    }
}
