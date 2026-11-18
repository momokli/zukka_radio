//! Minimal RFC5545-style recurrence subset (§3.2 of the plan).
//!
//! The rule is stored in `shows.recurrence` as a `;`-separated `KEY=VALUE`
//! list. `DTSTART` carries the Berlin wall-clock anchor (the schema has no
//! start column of its own):
//!
//! ```text
//! DTSTART=20260105T200000;FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;DURATION=PT2H
//! ```
//!
//! Expansion always iterates in Berlin wall time and only then maps each
//! occurrence through [`crate::time::berlin_to_utc`], so DST is handled by
//! the §3.5 policy instead of manual offset arithmetic.

use std::fmt;

use chrono::{
    DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ApiError;
use crate::time::berlin_to_utc;

/// Metadata check: the default name is fixed; the constant only documents it.
const TIMESTAMP_FORMAT: &str = "%Y%m%dT%H%M%S";
/// Hard guard against unbounded iteration (a rule without `COUNT`/`UNTIL`).
const MAX_PERIODS: u32 = 100_000;
/// Cap on returned occurrences per call (plan §3.2 step 5).
const MAX_OCCURRENCES: usize = 10_000;

/// Supported frequency (`FREQ`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
}

impl Freq {
    fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        match raw.to_ascii_uppercase().as_str() {
            "DAILY" => Ok(Freq::Daily),
            "WEEKLY" => Ok(Freq::Weekly),
            "MONTHLY" => Ok(Freq::Monthly),
            other => Err(ApiError::validation(
                field,
                format!("unsupported FREQ '{other}', expected DAILY, WEEKLY or MONTHLY"),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
        }
    }
}

/// One expanded occurrence: UTC start, optional UTC end, DST flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub start_utc: DateTime<Utc>,
    pub end_utc: Option<DateTime<Utc>>,
    pub dst_adjusted: bool,
}

/// Parsed recurrence rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceRule {
    /// Berlin wall-clock anchor (first occurrence).
    pub dtstart_local: NaiveDateTime,
    pub freq: Freq,
    /// `>= 1`; defaults to 1.
    pub interval: u32,
    /// `BYDAY` in Monday-first order; empty means "the DTSTART weekday".
    pub byday: Vec<Weekday>,
    pub count: Option<u32>,
    pub until_local: Option<NaiveDateTime>,
    pub until_utc: Option<DateTime<Utc>>,
    /// `None` means an open slot (`ends_at = NULL`).
    pub duration: Option<Duration>,
}

impl RecurrenceRule {
    /// Parses the storage form; any problem is a `422 validation_error` on
    /// `field` (always `"recurrence"` for shows).
    pub fn parse(field: &'static str, raw: &str) -> Result<Self, ApiError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ApiError::validation(
                field,
                "recurrence must not be empty (use null to clear)",
            ));
        }

        let mut dtstart: Option<NaiveDateTime> = None;
        let mut freq: Option<Freq> = None;
        let mut interval: u32 = 1;
        let mut byday: Vec<Weekday> = Vec::new();
        let mut count: Option<u32> = None;
        let mut until_local: Option<NaiveDateTime> = None;
        let mut until_utc: Option<DateTime<Utc>> = None;
        let mut duration: Option<Duration> = None;

        for part in raw.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (key, value) = part.split_once('=').ok_or_else(|| {
                ApiError::validation(field, format!("malformed recurrence part '{part}'"))
            })?;
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim();
            match key.as_str() {
                "DTSTART" => {
                    dtstart = Some(parse_local(field, "DTSTART", value)?);
                }
                "FREQ" => freq = Some(Freq::parse(field, value)?),
                "INTERVAL" => {
                    interval = value
                        .parse::<u32>()
                        .ok()
                        .filter(|v| *v >= 1)
                        .ok_or_else(|| {
                            ApiError::validation(
                                field,
                                format!("INTERVAL must be an integer >= 1, got '{value}'"),
                            )
                        })?;
                }
                "BYDAY" => byday = parse_byday(field, value)?,
                "COUNT" => {
                    count = Some(value.parse::<u32>().ok().filter(|v| *v >= 1).ok_or_else(
                        || {
                            ApiError::validation(
                                field,
                                format!("COUNT must be an integer >= 1, got '{value}'"),
                            )
                        },
                    )?);
                }
                "UNTIL" => {
                    if let Some(stripped) = value.strip_suffix('Z') {
                        let naive = NaiveDateTime::parse_from_str(stripped, TIMESTAMP_FORMAT)
                            .map_err(|_| invalid_timestamp(field, "UNTIL", value))?;
                        until_utc = Some(Utc.from_utc_datetime(&naive));
                    } else {
                        until_local = Some(parse_local(field, "UNTIL", value)?);
                    }
                }
                "DURATION" => duration = Some(parse_duration(field, value)?),
                other => {
                    return Err(ApiError::validation(
                        field,
                        format!("unknown recurrence key '{other}'"),
                    ));
                }
            }
        }

        let dtstart =
            dtstart.ok_or_else(|| ApiError::validation(field, "recurrence requires DTSTART"))?;
        let freq = freq.ok_or_else(|| ApiError::validation(field, "recurrence requires FREQ"))?;

        if count.is_some() && (until_local.is_some() || until_utc.is_some()) {
            return Err(ApiError::validation(
                field,
                "COUNT and UNTIL are mutually exclusive",
            ));
        }
        if !byday.is_empty() && freq == Freq::Daily {
            return Err(ApiError::validation(
                field,
                "BYDAY is only supported with WEEKLY or MONTHLY",
            ));
        }

        Ok(RecurrenceRule {
            dtstart_local: dtstart,
            freq,
            interval,
            byday,
            count,
            until_local,
            until_utc,
            duration,
        })
    }

    /// Canonical serialization used for storage and responses.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        let mut out = format!(
            "DTSTART={};FREQ={};INTERVAL={}",
            fmt_local(self.dtstart_local),
            self.freq.as_str(),
            self.interval
        );
        if !self.byday.is_empty() {
            out.push_str(";BYDAY=");
            out.push_str(&fmt_byday(&self.byday));
        }
        if let Some(count) = self.count {
            out.push_str(&format!(";COUNT={count}"));
        } else if let Some(until) = self.until_local {
            out.push_str(&format!(";UNTIL={}", fmt_local(until)));
        } else if let Some(until) = self.until_utc {
            out.push_str(&format!(";UNTIL={}Z", until.format(TIMESTAMP_FORMAT)));
        }
        if let Some(duration) = self.duration {
            out.push_str(&format!(";DURATION={}", fmt_duration(duration)));
        }
        out
    }

    /// Expands all occurrences that fall inside the UTC window (inclusive).
    pub fn occurrences(&self, from_utc: DateTime<Utc>, to_utc: DateTime<Utc>) -> Vec<Occurrence> {
        let mut out = Vec::new();
        // Number of occurrences at or after DTSTART, for COUNT.
        let mut seen: u32 = 0;

        'periods: for n in 0..MAX_PERIODS {
            for candidate in self.candidates(n) {
                if candidate < self.dtstart_local {
                    continue;
                }
                if let Some(count) = self.count {
                    if seen >= count {
                        break 'periods;
                    }
                }
                if let Some(until_local) = self.until_local {
                    if candidate > until_local {
                        break 'periods;
                    }
                }

                let (start_utc, start_adjusted) = berlin_to_utc(candidate, false);
                if let Some(until_utc) = self.until_utc {
                    if start_utc > until_utc {
                        break 'periods;
                    }
                }

                seen += 1;
                if start_utc > to_utc {
                    break 'periods;
                }
                if start_utc < from_utc {
                    continue;
                }

                let (end_utc, dst_adjusted) = match self.duration {
                    None => (None, start_adjusted),
                    Some(duration) => {
                        // Wall-clock addition: an overnight slot naturally
                        // lands on the next day (§3.2 step 4).
                        let end_local = candidate + duration;
                        let (mut end_utc, end_adjusted) = berlin_to_utc(end_local, true);
                        let mut adjusted = start_adjusted || end_adjusted;
                        if end_utc <= start_utc {
                            end_utc = start_utc + duration;
                            adjusted = true;
                        }
                        (Some(end_utc), adjusted)
                    }
                };

                out.push(Occurrence {
                    start_utc,
                    end_utc,
                    dst_adjusted,
                });
                if out.len() >= MAX_OCCURRENCES {
                    break 'periods;
                }
            }
        }

        out
    }

    /// Candidate wall-clock starts for period `n` (0-based).
    fn candidates(&self, n: u32) -> Vec<NaiveDateTime> {
        let step = (self.interval as i64).saturating_mul(n as i64);
        let time = self.dtstart_local.time();
        match self.freq {
            Freq::Daily => vec![self.dtstart_local + Duration::days(step)],
            Freq::Weekly => {
                let week_start = self.dtstart_local.date()
                    - Duration::days(weekday_index(self.dtstart_local.weekday()) as i64);
                let base = week_start + Duration::weeks(step);
                let days: Vec<Weekday> = if self.byday.is_empty() {
                    vec![self.dtstart_local.weekday()]
                } else {
                    self.byday.clone()
                };
                let mut candidates: Vec<NaiveDateTime> = days
                    .into_iter()
                    .map(|weekday| {
                        (base + Duration::days(weekday_index(weekday) as i64)).and_time(time)
                    })
                    .collect();
                candidates.sort();
                candidates
            }
            Freq::Monthly => {
                let date = self
                    .dtstart_local
                    .date()
                    .checked_add_months(Months::new(step as u32));
                match date {
                    Some(date) if self.month_matches(date) => vec![date.and_time(time)],
                    _ => Vec::new(),
                }
            }
        }
    }

    fn month_matches(&self, date: NaiveDate) -> bool {
        self.byday.is_empty() || self.byday.contains(&date.weekday())
    }
}

impl fmt::Display for RecurrenceRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&RecurrenceRule::to_string(self))
    }
}

impl Serialize for RecurrenceRule {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&RecurrenceRule::to_string(self))
    }
}

impl<'de> Deserialize<'de> for RecurrenceRule {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        RecurrenceRule::parse("recurrence", &raw).map_err(serde::de::Error::custom)
    }
}

fn invalid_timestamp(field: &'static str, key: &str, value: &str) -> ApiError {
    ApiError::validation(
        field,
        format!("{key} must be YYYYMMDDTHHMMSS, got '{value}'"),
    )
}

fn parse_local(field: &'static str, key: &str, value: &str) -> Result<NaiveDateTime, ApiError> {
    NaiveDateTime::parse_from_str(value, TIMESTAMP_FORMAT)
        .map_err(|_| invalid_timestamp(field, key, value))
}

fn parse_byday(field: &'static str, value: &str) -> Result<Vec<Weekday>, ApiError> {
    let mut days = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let weekday = parse_weekday(part)
            .ok_or_else(|| ApiError::validation(field, format!("invalid BYDAY token '{part}'")))?;
        if !days.contains(&weekday) {
            days.push(weekday);
        }
    }
    if days.is_empty() {
        return Err(ApiError::validation(
            field,
            "BYDAY must list at least one day",
        ));
    }
    days.sort_by_key(|weekday| weekday_index(*weekday));
    Ok(days)
}

fn parse_weekday(raw: &str) -> Option<Weekday> {
    match raw.to_ascii_uppercase().as_str() {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

fn weekday_index(weekday: Weekday) -> u32 {
    weekday.num_days_from_monday()
}

fn weekday_code(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

fn fmt_byday(days: &[Weekday]) -> String {
    days.iter()
        .map(|weekday| weekday_code(*weekday))
        .collect::<Vec<_>>()
        .join(",")
}

fn fmt_local(naive: NaiveDateTime) -> String {
    naive.format(TIMESTAMP_FORMAT).to_string()
}

/// Parses an ISO-8601 duration (`P1D`, `PT2H`, `PT90M`, `PT1H30M`).
fn parse_duration(field: &'static str, value: &str) -> Result<Duration, ApiError> {
    let upper = value.to_ascii_uppercase();
    let rest = upper.strip_prefix('P').ok_or_else(|| {
        ApiError::validation(field, format!("DURATION must be ISO-8601, got '{value}'"))
    })?;
    let (date_part, time_part) = match rest.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (rest, None),
    };

    let mut total = Duration::zero();
    total += parse_duration_part(field, date_part, false, value)?;
    if let Some(time_part) = time_part {
        if time_part.is_empty() {
            return Err(ApiError::validation(
                field,
                format!("DURATION has an empty time part: '{value}'"),
            ));
        }
        total += parse_duration_part(field, time_part, true, value)?;
    }

    if total <= Duration::zero() {
        return Err(ApiError::validation(
            field,
            "DURATION must be greater than zero",
        ));
    }
    Ok(total)
}

fn parse_duration_part(
    field: &'static str,
    part: &str,
    is_time: bool,
    original: &str,
) -> Result<Duration, ApiError> {
    let mut total = Duration::zero();
    let mut number = String::new();
    for ch in part.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let amount: i64 = number
            .parse()
            .map_err(|_| ApiError::validation(field, format!("invalid DURATION '{original}'")))?;
        number.clear();
        let increment = match (is_time, ch) {
            (false, 'D') => Duration::days(amount),
            (false, 'W') => Duration::weeks(amount),
            (true, 'H') => Duration::hours(amount),
            (true, 'M') => Duration::minutes(amount),
            (true, 'S') => Duration::seconds(amount),
            _ => {
                return Err(ApiError::validation(
                    field,
                    format!("unsupported DURATION component '{ch}' in '{original}'"),
                ));
            }
        };
        total += increment;
    }
    if !number.is_empty() {
        return Err(ApiError::validation(
            field,
            format!("DURATION component without unit in '{original}'"),
        ));
    }
    Ok(total)
}

fn fmt_duration(duration: Duration) -> String {
    let seconds = duration.num_seconds();
    if seconds % 86_400 == 0 {
        format!("P{}D", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("PT{}H", seconds / 3_600)
    } else if seconds % 60 == 0 {
        format!("PT{}M", seconds / 60)
    } else {
        format!("PT{seconds}S")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const RULE: &str = "DTSTART=20260105T200000;FREQ=WEEKLY;INTERVAL=1;BYDAY=WE,MO;DURATION=PT2H";

    #[test]
    fn parses_and_serializes_canonically() {
        let rule = RecurrenceRule::parse("recurrence", RULE).expect("parse");
        assert_eq!(rule.freq, Freq::Weekly);
        assert_eq!(rule.interval, 1);
        assert_eq!(rule.byday, vec![Weekday::Mon, Weekday::Wed]);
        assert_eq!(
            rule.to_string(),
            "DTSTART=20260105T200000;FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE;DURATION=PT2H"
        );
    }

    #[test]
    fn rejects_unknown_key() {
        let err = RecurrenceRule::parse("recurrence", "DTSTART=20260105T200000;FREQ=DAILY;FOO=1")
            .expect_err("unknown key");
        assert!(matches!(
            err,
            ApiError::Validation {
                field: "recurrence",
                ..
            }
        ));
    }

    #[test]
    fn rejects_count_and_until() {
        let err = RecurrenceRule::parse(
            "recurrence",
            "DTSTART=20260105T200000;FREQ=DAILY;COUNT=2;UNTIL=20260201T000000",
        )
        .expect_err("xor");
        assert!(matches!(err, ApiError::Validation { .. }));
    }

    #[test]
    fn weekly_byday_expands_sorted() {
        let rule = RecurrenceRule::parse("recurrence", RULE).expect("parse");
        let from = Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 1, 12, 0, 0, 0).unwrap();
        let occurrences = rule.occurrences(from, to);
        assert_eq!(occurrences.len(), 2, "Mon + Wed in the first week");
        assert!(occurrences[0].start_utc < occurrences[1].start_utc);
        assert_eq!(
            occurrences[0].start_utc,
            Utc.with_ymd_and_hms(2026, 1, 5, 19, 0, 0).unwrap()
        );
    }

    #[test]
    fn count_limits_occurrences() {
        let rule =
            RecurrenceRule::parse("recurrence", "DTSTART=20260105T200000;FREQ=DAILY;COUNT=3")
                .expect("parse");
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        assert_eq!(rule.occurrences(from, to).len(), 3);
    }

    #[test]
    fn monthly_clamps_to_month_end() {
        let rule =
            RecurrenceRule::parse("recurrence", "DTSTART=20260131T200000;FREQ=MONTHLY;COUNT=3")
                .expect("parse");
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let occurrences = rule.occurrences(from, to);
        assert_eq!(occurrences.len(), 3);
        // Jan 31, Feb 28 (clamped), Mar 31.
        let days: Vec<String> = occurrences
            .iter()
            .map(|o| o.start_utc.date_naive().to_string())
            .collect();
        assert_eq!(days, vec!["2026-01-31", "2026-02-28", "2026-03-31"]);
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(
            parse_duration("recurrence", "PT90M").unwrap(),
            Duration::minutes(90)
        );
        assert_eq!(
            parse_duration("recurrence", "P1DT2H").unwrap(),
            Duration::hours(26)
        );
        assert!(parse_duration("recurrence", "PT0S").is_err());
    }
}
