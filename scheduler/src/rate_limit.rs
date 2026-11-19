//! Sliding-window rate limiter for failed logins.
//!
//! Shared by the scheduler's `/internal/auth/verify` guard and the portal's
//! login handler. The window is configurable so tests can use a short one
//! instead of sleeping 15 minutes.

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

    /// Drops every entry (test helper).
    pub fn clear(&self) {
        let mut entries = self.entries.lock().expect("rate-limit mutex poisoned");
        entries.clear();
    }
}

fn prune(bucket: &mut Vec<Instant>, now: Instant, window: Duration) {
    bucket.retain(|at| now.duration_since(*at) < window);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_limit_and_resets_on_success() {
        let limiter = WindowLimiter::new(5, Duration::from_secs(900));
        for _ in 0..4 {
            limiter.record_failure("a");
        }
        assert!(!limiter.is_blocked("a"), "4 < 5 stays open");
        limiter.record_failure("a");
        assert!(limiter.is_blocked("a"), "5 failures block");
        assert!(!limiter.is_blocked("b"), "other keys are unaffected");
        limiter.reset("a");
        assert!(!limiter.is_blocked("a"), "success clears the window");
    }

    #[test]
    fn window_expires() {
        let limiter = WindowLimiter::new(5, Duration::from_millis(40));
        for _ in 0..5 {
            limiter.record_failure("a");
        }
        assert!(limiter.is_blocked("a"));
        std::thread::sleep(Duration::from_millis(70));
        assert!(!limiter.is_blocked("a"), "stale failures are pruned");
    }
}
