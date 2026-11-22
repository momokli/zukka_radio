//! Fader throttling (§6.5, Issue #148).
//!
//! Fader commands may arrive up to 50 Hz; the bridge collapses them to the last
//! value per channel and applies **at most 30 Hz** to the engine. A client that
//! exceeds 100 commands/s is answered with `rate_limited`; the rest of the burst
//! is dropped.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Hard rate limit per connection (§6.5).
pub const MAX_COMMANDS_PER_SECOND: u32 = 100;

/// Minimum interval between two engine applications of coalesced faders (30 Hz).
pub const FADER_APPLY_INTERVAL: Duration = Duration::from_millis(33);

/// Sliding-window command rate limiter (per connection).
#[derive(Debug)]
pub struct RateLimiter {
    window_start: Instant,
    count: u32,
}

impl RateLimiter {
    /// A fresh limiter whose window starts at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            count: 0,
        }
    }

    /// Records one command; `false` when the per-second budget is exhausted.
    pub fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        if self.count >= MAX_COMMANDS_PER_SECOND {
            return false;
        }
        self.count += 1;
        true
    }
}

/// Coalesces fader commands to the last value per channel, applied ≤30 Hz.
#[derive(Debug)]
pub struct FaderCoalescer {
    pending: HashMap<String, String>,
    last_flush: Instant,
}

impl FaderCoalescer {
    /// A fresh coalescer whose first flush window starts at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            pending: HashMap::new(),
            last_flush: now,
        }
    }

    /// Records the latest engine command for `key` (last value wins).
    pub fn submit(&mut self, key: impl Into<String>, command: impl Into<String>) {
        self.pending.insert(key.into(), command.into());
    }

    /// `true` when the pending commands may be applied to the engine.
    pub fn due(&self, now: Instant) -> bool {
        !self.pending.is_empty() && now.duration_since(self.last_flush) >= FADER_APPLY_INTERVAL
    }

    /// Drains and returns the coalesced engine commands when due (else empty).
    pub fn take(&mut self, now: Instant) -> Vec<String> {
        if !self.due(now) {
            return Vec::new();
        }
        self.last_flush = now;
        self.pending.drain().map(|(_, command)| command).collect()
    }

    /// Number of commands currently waiting to be applied.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_100_then_rejects_burst() {
        let start = Instant::now();
        let mut limiter = RateLimiter::new(start);
        for _ in 0..MAX_COMMANDS_PER_SECOND {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start));
        // New window resets the budget.
        assert!(limiter.allow(start + Duration::from_millis(1000)));
    }

    #[test]
    fn coalescer_collapses_last_value_per_channel() {
        let start = Instant::now();
        let mut coalescer = FaderCoalescer::new(start);
        coalescer.submit("mixer.set live", "mixer.set live -6");
        coalescer.submit("mixer.set live", "mixer.set live -3");
        coalescer.submit("mixer.set show", "mixer.set show -9");
        assert_eq!(coalescer.pending_len(), 2);
        assert!(!coalescer.due(start));
        let applied = coalescer.take(start + FADER_APPLY_INTERVAL);
        assert_eq!(applied.len(), 2);
        assert!(applied.contains(&"mixer.set live -3".to_string()));
        assert!(applied.contains(&"mixer.set show -9".to_string()));
        assert_eq!(coalescer.pending_len(), 0);
    }

    #[test]
    fn coalescer_not_due_before_interval() {
        let start = Instant::now();
        let mut coalescer = FaderCoalescer::new(start);
        coalescer.submit("mixer.master", "mixer.master -1");
        assert!(coalescer.take(start + Duration::from_millis(10)).is_empty());
        assert_eq!(coalescer.pending_len(), 1);
    }
}
