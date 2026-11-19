//! Sliding-window rate limiter (login: 5 failures / 15 min / IP+username).
//!
//! Mirrors the scheduler's limiter; the window is configurable so tests can use
//! a short one instead of sleeping 15 minutes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Counts recent failures per key and blocks once the limit is reached.
pub struct WindowLimiter {
    max_attempts: usize,
    window: Duration,
    entries: Mutex<HashMap<String, Vec<Instant>>>,
}

impl WindowLimiter {
    /// `max_attempts` failures inside `window` block the key.
    pub fn new(max_attempts: usize, window: Duration) -> Self {
        Self {
            max_attempts,
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// `true` when `key` reached the limit within the current window.
    pub fn is_blocked(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("rate-limit mutex poisoned");
        let bucket = entries.entry(key.to_string()).or_default();
        prune(bucket, now, self.window);
        bucket.len() >= self.max_attempts
    }

    /// Records one failed attempt for `key`.
    pub fn record_failure(&self, key: &str) {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("rate-limit mutex poisoned");
        let bucket = entries.entry(key.to_string()).or_default();
        prune(bucket, now, self.window);
        bucket.push(now);
    }

    /// Clears the failures of `key` (e.g. after a successful login).
    pub fn reset(&self, key: &str) {
        let mut entries = self.entries.lock().expect("rate-limit mutex poisoned");
        entries.remove(key);
    }
}

fn prune(bucket: &mut Vec<Instant>, now: Instant, window: Duration) {
    bucket.retain(|at| now.duration_since(*at) < window);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_five_and_resets() {
        let limiter = WindowLimiter::new(5, Duration::from_secs(900));
        for _ in 0..5 {
            limiter.record_failure("a|1.2.3.4");
        }
        assert!(limiter.is_blocked("a|1.2.3.4"));
        assert!(!limiter.is_blocked("a|5.6.7.8"), "keyed by ip too");
        limiter.reset("a|1.2.3.4");
        assert!(!limiter.is_blocked("a|1.2.3.4"));
    }

    #[test]
    fn window_expires() {
        let limiter = WindowLimiter::new(5, Duration::from_millis(40));
        for _ in 0..5 {
            limiter.record_failure("k");
        }
        assert!(limiter.is_blocked("k"));
        std::thread::sleep(Duration::from_millis(70));
        assert!(!limiter.is_blocked("k"));
    }
}
