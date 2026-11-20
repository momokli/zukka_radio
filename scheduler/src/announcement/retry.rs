//! Pure retry policy for announcements (Issue #35).
//!
//! Only arithmetic lives here — no I/O, no clock. The store layer applies the
//! result (`next_attempt_at = at + backoff(attempts)`), the poll loop is #37.

use chrono::Duration;

/// Number of failed attempts after which an announcement becomes terminal
/// (`status = 'failed'`).
pub const MAX_ATTEMPTS: i64 = 5;

/// Base delay: the first retry waits this long.
pub const BASE_BACKOFF_SECS: i64 = 30;

/// Hard ceiling for the exponential backoff.
pub const MAX_BACKOFF_SECS: i64 = 3600;

/// Exponential backoff: `30s, 60s, 120s, …`, capped at
/// [`MAX_BACKOFF_SECS`].
///
/// `attempts` is the number of **already failed** attempts (`>= 1` after the
/// first failure). `attempts <= 0` (no failure yet) yields a zero delay.
pub fn backoff(attempts: i64) -> Duration {
    if attempts <= 0 {
        return Duration::seconds(0);
    }
    // Avoid shifting past `i64` width; the cap already dominates far earlier.
    let shift = (attempts - 1).min(30) as u32;
    let secs = BASE_BACKOFF_SECS
        .saturating_mul(1_i64 << shift)
        .min(MAX_BACKOFF_SECS);
    Duration::seconds(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        let secs: Vec<i64> = (1..=8).map(|n| backoff(n).num_seconds()).collect();
        assert_eq!(secs, vec![30, 60, 120, 240, 480, 960, 1920, 3600]);
    }

    #[test]
    fn backoff_is_monotonic() {
        let mut prev = backoff(0).num_seconds();
        for n in 1..40 {
            let cur = backoff(n).num_seconds();
            assert!(cur >= prev, "not monotonic at {n}: {prev} -> {cur}");
            prev = cur;
        }
    }

    #[test]
    fn zero_or_negative_is_zero() {
        assert_eq!(backoff(0), Duration::seconds(0));
        assert_eq!(backoff(-5), Duration::seconds(0));
    }

    #[test]
    fn max_attempts_constant() {
        assert_eq!(MAX_ATTEMPTS, 5);
    }
}
