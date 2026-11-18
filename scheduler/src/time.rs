//! Time helpers: Europe/Berlin ↔ UTC and the §3.5 DST policy.
//!
//! Storage is always UTC (`YYYY-MM-DDTHH:MM:SS.sssZ`); the API speaks Berlin
//! wall time with an explicit offset. Wall-clock (naive) values are resolved
//! against `Europe/Berlin` so DST gaps/overlaps follow §3.5:
//!
//! | Case | Behaviour |
//! | --- | --- |
//! | Spring forward (local time does not exist) | first valid instant after the gap, `dst_adjusted = true` |
//! | Fall back, start | earlier occurrence (+02:00) |
//! | Fall back, end | later occurrence (+01:00) |

use chrono::{DateTime, Duration, MappedLocalTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use chrono_tz::Tz;

use crate::error::ApiError;

/// The timezone every wall-clock value in the scheduler is anchored in.
pub const BERLIN: Tz = Berlin;

/// Resolves a Berlin wall-clock time to UTC.
///
/// `is_end` selects the later occurrence for ambiguous fall-back times; for a
/// start the earlier occurrence wins (§3.5). Nonexistent spring-forward times
/// are shifted forward out of the gap. The boolean is `dst_adjusted`.
pub fn berlin_to_utc(naive: NaiveDateTime, is_end: bool) -> (DateTime<Utc>, bool) {
    match Berlin.from_local_datetime(&naive) {
        MappedLocalTime::Single(dt) => (dt.with_timezone(&Utc), false),
        MappedLocalTime::Ambiguous(earlier, later) => {
            let dt = if is_end { later } else { earlier };
            (dt.with_timezone(&Utc), false)
        }
        MappedLocalTime::None => {
            // Spring-forward gap (1h in Europe/Berlin): step forward out of
            // the gap until the wall clock exists again.
            let mut candidate = naive;
            for _ in 0..4 {
                candidate += Duration::hours(1);
                if let MappedLocalTime::Single(dt) = Berlin.from_local_datetime(&candidate) {
                    return (dt.with_timezone(&Utc), true);
                }
            }
            (Utc.from_utc_datetime(&naive), true)
        }
    }
}

/// Converts a UTC instant to Berlin local time (with offset).
pub fn utc_to_berlin(utc: DateTime<Utc>) -> DateTime<Tz> {
    utc.with_timezone(&BERLIN)
}

/// Parses an API timestamp. RFC3339 offsets are honoured; a value without an
/// offset is interpreted as Berlin wall time (§3.5).
pub fn parse_api_time(field: &'static str, raw: &str) -> Result<DateTime<Utc>, ApiError> {
    let raw = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }

    const FORMATS: [&str; 3] = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ];
    for fmt in FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Ok(berlin_to_utc(naive, false).0);
        }
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is always a valid time");
        return Ok(berlin_to_utc(naive, false).0);
    }

    Err(ApiError::bad_request(format!(
        "invalid {field}: expected an RFC3339 timestamp, got '{raw}'"
    )))
}

/// Formats a UTC instant as Berlin wall time with explicit offset
/// (`2026-11-18T20:00:00+01:00`).
pub fn fmt_api_time(utc: DateTime<Utc>) -> String {
    utc.with_timezone(&BERLIN)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(raw: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S").expect("naive time")
    }

    #[test]
    fn plain_winter_time_roundtrip() {
        let (utc, adjusted) = berlin_to_utc(naive("2026-11-18T20:00:00"), false);
        assert!(!adjusted);
        assert_eq!(fmt_storage_time(utc), "2026-11-18T19:00:00.000Z");
        assert_eq!(fmt_api_time(utc), "2026-11-18T20:00:00+01:00");
    }

    #[test]
    fn spring_forward_gap_is_adjusted() {
        let (utc, adjusted) = berlin_to_utc(naive("2026-03-29T02:30:00"), false);
        assert!(adjusted);
        assert_eq!(fmt_storage_time(utc), "2026-03-29T01:30:00.000Z");
    }

    #[test]
    fn fall_back_start_takes_earlier_occurrence() {
        let (utc, adjusted) = berlin_to_utc(naive("2026-10-25T02:30:00"), false);
        assert!(!adjusted);
        assert_eq!(
            fmt_api_time(utc),
            "2026-10-25T02:30:00+02:00",
            "start must use the summer offset"
        );
    }

    #[test]
    fn fall_back_end_takes_later_occurrence() {
        let (utc, _) = berlin_to_utc(naive("2026-10-25T02:30:00"), true);
        assert_eq!(
            fmt_api_time(utc),
            "2026-10-25T02:30:00+01:00",
            "end must use the winter offset"
        );
    }

    #[test]
    fn parse_without_offset_is_berlin() {
        let utc = parse_api_time("starts_at", "2026-11-18T20:00:00").expect("parse");
        assert_eq!(fmt_storage_time(utc), "2026-11-18T19:00:00.000Z");
    }

    #[test]
    fn parse_with_offset_is_absolute() {
        let utc = parse_api_time("starts_at", "2026-11-18T20:00:00Z").expect("parse");
        assert_eq!(fmt_api_time(utc), "2026-11-18T21:00:00+01:00");
    }

    #[test]
    fn parse_rejects_garbage() {
        let err = parse_api_time("starts_at", "not-a-time").expect_err("must fail");
        assert!(matches!(err, ApiError::BadRequest { .. }));
    }
}
