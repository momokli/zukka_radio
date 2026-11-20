//! Time helpers: Europe/Berlin ↔ UTC (`docs/tapes.md` §3.5, §3.2 D5).
//!
//! Storage is RFC3339 UTC with milliseconds (`…sssZ`). The API emits UTC `…Z`
//! for `started_at`/`ended_at`/`server_time` and Berlin wall time with an
//! explicit offset for `start_local`/`end_local`/`window_label`. Ported (not
//! imported) from `scheduler/src/time.rs`.

use chrono::{DateTime, Days, MappedLocalTime, NaiveDate, SecondsFormat, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use chrono_tz::Tz;

use crate::error::ApiError;

/// The timezone all wall-clock display values are anchored in.
pub const BERLIN: Tz = Berlin;

/// Parses the canonical storage form (`YYYY-MM-DDTHH:MM:SS.sssZ`).
pub fn parse_storage_time(raw: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| {
            ApiError::Internal(anyhow::anyhow!("invalid stored timestamp '{raw}': {err}"))
        })
}

/// Formats a UTC instant in the canonical storage form (millisecond UTC).
pub fn fmt_storage_time(utc: DateTime<Utc>) -> String {
    utc.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Formats a UTC instant as the API storage form (`2026-11-23T10:00:00Z`).
pub fn fmt_utc_z(utc: DateTime<Utc>) -> String {
    utc.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Formats a UTC instant as Berlin wall time with an explicit offset
/// (`2026-11-23T11:00:00+01:00`).
pub fn fmt_api_time(utc: DateTime<Utc>) -> String {
    utc.with_timezone(&BERLIN)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// Builds the human window label in Berlin time (`23.11.2026 11:00 – 13:00`).
pub fn window_label(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    let start = start.with_timezone(&BERLIN);
    let end = end.with_timezone(&BERLIN);
    format!(
        "{} \u{2013} {}",
        start.format("%d.%m.%Y %H:%M"),
        end.format("%H:%M")
    )
}

/// Start of the Berlin calendar day `date`, as a UTC instant.
///
/// `from`/`to` query parameters are Berlin calendar days, so the UTC window is
/// `[00:00 Berlin from, 00:00 Berlin (to + 1 day))`.
pub fn berlin_day_start_utc(date: NaiveDate) -> DateTime<Utc> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time");
    match BERLIN.from_local_datetime(&naive) {
        MappedLocalTime::Single(dt) => dt.with_timezone(&Utc),
        MappedLocalTime::Ambiguous(earlier, _) => earlier.with_timezone(&Utc),
        MappedLocalTime::None => {
            // Midnight never falls in the DST gap in Europe/Berlin, but step
            // out defensively if it ever did.
            let mut candidate = naive;
            for _ in 0..4 {
                candidate += chrono::Duration::hours(1);
                if let MappedLocalTime::Single(dt) = BERLIN.from_local_datetime(&candidate) {
                    return dt.with_timezone(&Utc);
                }
            }
            Utc.from_utc_datetime(&naive)
        }
    }
}

/// `berlin_day_start_utc(date)` for the day *after* `date` (exclusive upper bound).
pub fn berlin_day_end_utc(date: NaiveDate) -> DateTime<Utc> {
    berlin_day_start_utc(date + Days::new(1))
}

/// Parses a `YYYY-MM-DD` query parameter; invalid input → `400`.
pub fn parse_date(field: &str, raw: &str) -> Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| {
        ApiError::bad_request(format!(
            "invalid {field}: expected a YYYY-MM-DD date, got '{raw}'"
        ))
    })
}
