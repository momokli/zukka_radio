//! Berlin ↔ UTC helpers for the schedule UI (portal.md D13, §3.2).
//!
//! Storage and every API call speak UTC; every label and form field speaks
//! `Europe/Berlin`. This is a slim subset of `scheduler/src/time.rs` plus the
//! calendar-range maths the portal needs on its own (the `/schedule` endpoint
//! returns entries, not navigation metadata — plan "Offene Punkte").
//!
//! DST policy mirrors the scheduler (§3.5): a nonexistent spring-forward wall
//! time is shifted forward out of the gap, an ambiguous fall-back start takes
//! the earlier occurrence (ends the later one).

use chrono::{
    DateTime, Datelike, Duration, MappedLocalTime, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use chrono_tz::Europe::Berlin;
use chrono_tz::Tz;

/// The timezone every wall-clock value in the UI is anchored in.
pub const BERLIN: Tz = Berlin;

use chrono::SecondsFormat;

/// Resolves a Berlin wall-clock time to UTC (`is_end` selects the later
/// occurrence for ambiguous fall-back times). The boolean is `dst_adjusted`.
pub fn berlin_to_utc(naive: NaiveDateTime, is_end: bool) -> (DateTime<Utc>, bool) {
    match Berlin.from_local_datetime(&naive) {
        MappedLocalTime::Single(dt) => (dt.with_timezone(&Utc), false),
        MappedLocalTime::Ambiguous(earlier, later) => {
            let dt = if is_end { later } else { earlier };
            (dt.with_timezone(&Utc), false)
        }
        MappedLocalTime::None => {
            // Spring-forward gap: step forward until the wall clock exists.
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

/// Converts a UTC instant to Berlin local time.
pub fn to_berlin(utc: DateTime<Utc>) -> DateTime<Tz> {
    utc.with_timezone(&BERLIN)
}

/// Parses an API/storage timestamp (RFC3339, with offset or `Z`).
pub fn parse_api(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Formats a UTC instant as a Berlin `datetime-local` value (`YYYY-MM-DDTHH:MM`).
pub fn fmt_input(utc: DateTime<Utc>) -> String {
    to_berlin(utc).format("%Y-%m-%dT%H:%M").to_string()
}

/// Parses a Berlin `datetime-local` value (`YYYY-MM-DDTHH:MM[:SS]`).
pub fn parse_input(raw: &str) -> Option<NaiveDateTime> {
    let raw = raw.trim();
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(naive);
        }
    }
    None
}

/// Formats a UTC instant as an RFC3339 UTC string with a `Z` suffix.
pub fn fmt_utc(utc: DateTime<Utc>) -> String {
    utc.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `HH:MM` in Berlin.
pub fn fmt_hm(utc: DateTime<Utc>) -> String {
    to_berlin(utc).format("%H:%M").to_string()
}

/// Today's date in Berlin.
pub fn today_berlin() -> NaiveDate {
    to_berlin(Utc::now()).date_naive()
}

/// The two-letter German weekday code (ISO Monday-first).
pub fn weekday_code(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Mo",
        Weekday::Tue => "Di",
        Weekday::Wed => "Mi",
        Weekday::Thu => "Do",
        Weekday::Fri => "Fr",
        Weekday::Sat => "Sa",
        Weekday::Sun => "So",
    }
}

/// The ISO week number of `date`.
pub fn iso_week(date: NaiveDate) -> u32 {
    date.iso_week().week()
}

// ---------------------------------------------------------------------------
// Calendar ranges (§3.2): view=week = ISO Mon–So, view=month = calendar month.
// ---------------------------------------------------------------------------

/// Which calendar the UI renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarView {
    Week,
    Month,
}

impl CalendarView {
    /// Parses `?view=`; anything but `month` is a week.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("week") {
            "month" => CalendarView::Month,
            _ => CalendarView::Week,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CalendarView::Week => "week",
            CalendarView::Month => "month",
        }
    }

    /// The Monday starting the ISO week containing `date`.
    fn week_start(date: NaiveDate) -> NaiveDate {
        date - Duration::days(date.weekday().num_days_from_monday() as i64)
    }

    /// The UTC window `[from, to)` covering the visible grid.
    pub fn range(self, anchor: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
        match self {
            CalendarView::Week => {
                let start = Self::week_start(anchor);
                let end = start + Duration::days(7);
                (midnight_utc(start), midnight_utc(end))
            }
            CalendarView::Month => {
                let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1)
                    .expect("first of month");
                let end = first
                    .checked_add_months(chrono::Months::new(1))
                    .expect("next month");
                (midnight_utc(first), midnight_utc(end))
            }
        }
    }

    /// Every Berlin day to render, week-major (7 per week).
    pub fn grid(self, anchor: NaiveDate) -> Vec<NaiveDate> {
        match self {
            CalendarView::Week => {
                let start = Self::week_start(anchor);
                (0..7).map(|i| start + Duration::days(i)).collect()
            }
            CalendarView::Month => {
                let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1)
                    .expect("first of month");
                let next = first
                    .checked_add_months(chrono::Months::new(1))
                    .expect("next month");
                let start = Self::week_start(first);
                let last = next - Duration::days(1);
                let end = Self::week_start(last) + Duration::days(7);
                let mut days = Vec::new();
                let mut day = start;
                while day < end {
                    days.push(day);
                    day += Duration::days(1);
                }
                days
            }
        }
    }

    /// The anchor after shifting by `dir` (−1 previous, +1 next).
    pub fn shift(self, anchor: NaiveDate, dir: i32) -> NaiveDate {
        match self {
            CalendarView::Week => anchor + Duration::weeks(1) * dir,
            CalendarView::Month => {
                let month0 = anchor.year() as i64 * 12 + (anchor.month() as i64 - 1) + dir as i64;
                let year = (month0.div_euclid(12)) as i32;
                let month = (month0.rem_euclid(12) + 1) as u32;
                NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(anchor)
            }
        }
    }

    /// A short header title for the visible grid.
    pub fn title(self, anchor: NaiveDate) -> String {
        match self {
            CalendarView::Week => {
                let start = Self::week_start(anchor);
                let end = start + Duration::days(6);
                format!(
                    "KW {} · {:02}.{:02}. – {:02}.{:02}.{}",
                    iso_week(anchor),
                    start.day(),
                    start.month(),
                    end.day(),
                    end.month(),
                    end.year()
                )
            }
            CalendarView::Month => {
                const MONTHS: [&str; 12] = [
                    "Januar",
                    "Februar",
                    "März",
                    "April",
                    "Mai",
                    "Juni",
                    "Juli",
                    "August",
                    "September",
                    "Oktober",
                    "November",
                    "Dezember",
                ];
                format!(
                    "{} {}",
                    MONTHS[(anchor.month() as usize) - 1],
                    anchor.year()
                )
            }
        }
    }
}

/// Midnight Berlin on `date`, in UTC.
fn midnight_utc(date: NaiveDate) -> DateTime<Utc> {
    let naive = date.and_hms_opt(0, 0, 0).expect("midnight");
    berlin_to_utc(naive, false).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn winter_roundtrip() {
        let naive = parse_input("2026-11-18T20:00").unwrap();
        let (utc, adjusted) = berlin_to_utc(naive, false);
        assert!(!adjusted);
        assert_eq!(fmt_utc(utc), "2026-11-18T19:00:00Z");
        assert_eq!(fmt_input(utc), "2026-11-18T20:00");
    }

    #[test]
    fn spring_forward_gap_is_adjusted() {
        let naive = parse_input("2026-03-29T02:30").unwrap();
        let (utc, adjusted) = berlin_to_utc(naive, false);
        assert!(adjusted);
        assert_eq!(fmt_utc(utc), "2026-03-29T01:30:00Z");
    }

    #[test]
    fn fall_back_end_takes_later_occurrence() {
        let naive = parse_input("2026-10-25T02:30").unwrap();
        let (utc, _) = berlin_to_utc(naive, true);
        assert_eq!(fmt_hm(utc), "02:30");
        let (start, _) = berlin_to_utc(naive, false);
        assert!(start < utc, "end uses the winter offset");
    }

    #[test]
    fn week_range_is_monday_to_monday() {
        // 2026-11-18 is a Wednesday.
        let (from, to) = CalendarView::Week.range(date(2026, 11, 18));
        assert_eq!(fmt_input(from), "2026-11-16T00:00", "Monday");
        assert_eq!(fmt_input(to), "2026-11-23T00:00", "next Monday");
    }

    #[test]
    fn week_grid_has_seven_days() {
        let days = CalendarView::Week.grid(date(2026, 11, 18));
        assert_eq!(days.len(), 7);
        assert_eq!(days[0], date(2026, 11, 16));
        assert_eq!(days[6], date(2026, 11, 22));
    }

    #[test]
    fn month_grid_starts_on_monday_and_covers_month() {
        // November 2026: 1st is a Sunday → grid starts Mon 2026-10-26.
        let days = CalendarView::Month.grid(date(2026, 11, 15));
        assert_eq!(days[0], date(2026, 10, 26));
        assert_eq!(days.len() % 7, 0);
        assert!(days.contains(&date(2026, 11, 1)));
        assert!(days.contains(&date(2026, 11, 30)));
    }

    #[test]
    fn month_range_is_calendar_month() {
        let (from, to) = CalendarView::Month.range(date(2026, 11, 15));
        assert_eq!(fmt_input(from), "2026-11-01T00:00");
        assert_eq!(fmt_input(to), "2026-12-01T00:00");
    }

    #[test]
    fn month_shift_wraps_year() {
        assert_eq!(
            CalendarView::Month.shift(date(2026, 12, 15), 1),
            date(2027, 1, 1)
        );
        assert_eq!(
            CalendarView::Month.shift(date(2026, 1, 15), -1),
            date(2025, 12, 1)
        );
    }

    #[test]
    fn month_grid_over_dst_fall_back_keeps_midnight() {
        // October 2026: DST ends 2026-10-25. Midnight labels stay 00:00.
        let (from, to) = CalendarView::Month.range(date(2026, 10, 15));
        assert_eq!(fmt_input(from), "2026-10-01T00:00");
        assert_eq!(fmt_input(to), "2026-11-01T00:00");
    }
}
