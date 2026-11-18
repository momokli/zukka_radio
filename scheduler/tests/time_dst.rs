//! DST edge cases for the §3.5 time model (Issue #11).
//!
//! These tests pin the Europe/Berlin resolution rules directly on
//! `zukka_scheduler::time`: spring-forward gaps, fall-back ambiguity, and
//! overnight wall-clock arithmetic.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use zukka_scheduler::time::{berlin_to_utc, fmt_api_time, fmt_storage_time, parse_api_time};

fn naive(raw: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S").expect("naive time")
}

#[test]
fn spring_forward_nonexistent_time_shifts_forward() {
    // 2026-03-29 02:30 does not exist in Europe/Berlin (02:00 → 03:00).
    let (utc, dst_adjusted) = berlin_to_utc(naive("2026-03-29T02:30:00"), false);
    assert!(dst_adjusted, "gap time must report dst_adjusted");
    assert_eq!(fmt_storage_time(utc), "2026-03-29T01:30:00.000Z");
    assert_eq!(fmt_api_time(utc), "2026-03-29T03:30:00+02:00");
}

#[test]
fn fall_back_start_uses_earlier_occurrence() {
    // 2026-10-25 02:30 happens twice: +02:00 (00:30Z) and +01:00 (01:30Z).
    let (utc, dst_adjusted) = berlin_to_utc(naive("2026-10-25T02:30:00"), false);
    assert!(!dst_adjusted);
    assert_eq!(fmt_api_time(utc), "2026-10-25T02:30:00+02:00");
    assert_eq!(fmt_storage_time(utc), "2026-10-25T00:30:00.000Z");
}

#[test]
fn fall_back_end_uses_later_occurrence() {
    let (utc, dst_adjusted) = berlin_to_utc(naive("2026-10-25T02:30:00"), true);
    assert!(!dst_adjusted);
    assert_eq!(fmt_api_time(utc), "2026-10-25T02:30:00+01:00");
    assert_eq!(fmt_storage_time(utc), "2026-10-25T01:30:00.000Z");
}

#[test]
fn overnight_duration_lands_on_next_day() {
    let start = naive("2026-11-18T20:00:00");
    let end = start + Duration::hours(6);
    assert_eq!(
        end,
        NaiveDate::from_ymd_opt(2026, 11, 19)
            .unwrap()
            .and_hms_opt(2, 0, 0)
            .unwrap()
    );
    let (start_utc, _) = berlin_to_utc(start, false);
    let (end_utc, _) = berlin_to_utc(end, true);
    assert_eq!(fmt_api_time(start_utc), "2026-11-18T20:00:00+01:00");
    assert_eq!(fmt_api_time(end_utc), "2026-11-19T02:00:00+01:00");
    assert!(end_utc > start_utc);
}

#[test]
fn api_time_without_offset_is_berlin() {
    let utc = parse_api_time("starts_at", "2026-11-18T20:00:00").unwrap();
    assert_eq!(fmt_api_time(utc), "2026-11-18T20:00:00+01:00");
}

#[test]
fn api_time_with_offset_is_respected() {
    let utc = parse_api_time("starts_at", "2026-11-18T20:00:00+01:00").unwrap();
    assert_eq!(fmt_storage_time(utc), "2026-11-18T19:00:00.000Z");
}

#[test]
fn fmt_api_time_always_carries_offset() {
    let utc = parse_api_time("starts_at", "2026-07-01T12:00:00Z").unwrap();
    let formatted = fmt_api_time(utc);
    assert!(
        formatted.ends_with("+02:00"),
        "summer time must format with +02:00, got {formatted}"
    );
}
