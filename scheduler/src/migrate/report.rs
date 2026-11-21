//! Migration report: per-entity counters + issue list (Issue #46, Story 5).
//!
//! Every entity reports `read`, `written` (dry-run: *would write*), `skipped`
//! (already present and unchanged) and `failed`. The counting invariant
//! `read == written + skipped + failed` must always hold; a violation is a bug
//! and is surfaced as an error before anything is written.

use std::fmt::Write as _;

/// A single rejected legacy row (orphan, duplicate or invalid field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Entity name: `caster`, `track` or `tape`.
    pub entity: &'static str,
    /// The offending natural/legacy key.
    pub key: String,
    /// Human-readable reason.
    pub reason: String,
}

impl Issue {
    pub fn new(entity: &'static str, key: impl Into<String>, reason: impl Into<String>) -> Self {
        Issue {
            entity,
            key: key.into(),
            reason: reason.into(),
        }
    }
}

/// Counters for one entity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityReport {
    pub read: usize,
    pub written: usize,
    pub skipped: usize,
    pub failed: usize,
}

impl EntityReport {
    /// True when the counting invariant holds.
    pub fn invariant_ok(&self) -> bool {
        self.read == self.written + self.skipped + self.failed
    }
}

/// Aggregate report for the whole migration.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub casters: EntityReport,
    pub tracks: EntityReport,
    pub tapes: EntityReport,
    pub issues: Vec<Issue>,
}

impl Report {
    /// True when every entity satisfies the counting invariant.
    pub fn invariant_ok(&self) -> bool {
        self.casters.invariant_ok() && self.tracks.invariant_ok() && self.tapes.invariant_ok()
    }

    /// True when at least one row was rejected.
    pub fn has_issues(&self) -> bool {
        !self.issues.is_empty()
    }

    /// Renders a human-readable report for stdout.
    pub fn render(&self, applied: bool) -> String {
        let verb = if applied { "written" } else { "would write" };
        let mut out = String::new();
        let _ = writeln!(out, "entity   read  {verb:<11} skipped  failed");
        for (name, r) in [
            ("caster", &self.casters),
            ("track", &self.tracks),
            ("tape", &self.tapes),
        ] {
            let _ = writeln!(
                out,
                "{name:<8} {:>4}  {:>11} {:>7} {:>7}",
                r.read, r.written, r.skipped, r.failed
            );
        }
        if !self.issues.is_empty() {
            let _ = writeln!(out, "\nissues ({}):", self.issues.len());
            for issue in &self.issues {
                let _ = writeln!(out, "  - {} {}: {}", issue.entity, issue.key, issue.reason);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_detects_mismatch() {
        let ok = EntityReport {
            read: 3,
            written: 2,
            skipped: 1,
            failed: 0,
        };
        assert!(ok.invariant_ok());

        let bad = EntityReport {
            read: 3,
            written: 2,
            skipped: 0,
            failed: 0,
        };
        assert!(!bad.invariant_ok());
    }
}
