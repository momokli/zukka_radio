//! Schedule store: conflict detection (§4), window merge and expansion.
//!
//! This module owns the shared overlap rules; the broadcast store calls
//! [`detect_conflicts`] for hard self-overlaps and soft warnings.

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;

use crate::dto::{BroadcastKind, WarningDto};
use crate::error::ApiResult;
use crate::time::{fmt_api_time, fmt_storage_time, parse_storage_time};

/// A candidate slot used for conflict checks (id `None` before insert).
pub(crate) struct Slot<'a> {
    pub id: Option<&'a str>,
    pub show_id: Option<&'a str>,
    pub caster_id: &'a str,
    pub kind: BroadcastKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
}

/// Result of [`detect_conflicts`].
pub(crate) struct ConflictReport {
    /// `(show_id, conflicting_broadcast_id)` when the show overlaps itself.
    pub show_overlap: Option<(String, String)>,
    pub warnings: Vec<WarningDto>,
}

/// Interval overlap: `[a.starts, a.ends)` vs `[b.starts, b.ends)`, open end = ∞.
pub(crate) fn overlaps(
    a_start: DateTime<Utc>,
    a_end: Option<DateTime<Utc>>,
    b_start: DateTime<Utc>,
    b_end: Option<DateTime<Utc>>,
) -> bool {
    let a_before_b_end = b_end.is_none_or(|b_end| a_start < b_end);
    let b_before_a_end = a_end.is_none_or(|a_end| b_start < a_end);
    a_before_b_end && b_before_a_end
}

/// Finds overlapping non-cancelled broadcasts and classifies them (§4.1/§4.2).
///
/// The first self-overlap of the same `show_id` is a hard conflict; foreign
/// shows, simultaneous live slots and caster double-booking are warnings.
pub(crate) async fn detect_conflicts(
    conn: &mut SqliteConnection,
    slot: &Slot<'_>,
) -> ApiResult<ConflictReport> {
    let start = fmt_storage_time(slot.starts_at);
    let end = slot.ends_at.map(fmt_storage_time);

    let rows: Vec<OverlapRow> = sqlx::query_as(
        "SELECT id, show_id, caster_id, kind, title, starts_at, ends_at FROM broadcasts \
         WHERE status != 'cancelled' \
           AND (? IS NULL OR id != ?) \
           AND (? IS NULL OR starts_at < ?) \
           AND (ends_at IS NULL OR ends_at > ?) \
         ORDER BY starts_at, id",
    )
    .bind(slot.id)
    .bind(slot.id)
    .bind(&end)
    .bind(&end)
    .bind(&start)
    .fetch_all(&mut *conn)
    .await?;

    let mut report = ConflictReport {
        show_overlap: None,
        warnings: Vec::new(),
    };

    for row in rows {
        let other_start = parse_storage_time(&row.starts_at)?;
        let other_end = match &row.ends_at {
            Some(raw) => Some(parse_storage_time(raw)?),
            None => None,
        };
        // The SQL filter already enforces the overlap; re-check in Rust so the
        // open-end rule is stated in exactly one place.
        if !overlaps(slot.starts_at, slot.ends_at, other_start, other_end) {
            continue;
        }

        let same_show = match (slot.show_id, row.show_id.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if same_show && report.show_overlap.is_none() {
            report.show_overlap =
                Some((slot.show_id.unwrap_or_default().to_string(), row.id.clone()));
            continue;
        }

        let mut push = |code: &str, message: String| {
            report.warnings.push(WarningDto {
                code: code.to_string(),
                message,
                broadcast_id: Some(row.id.clone()),
                show_id: row.show_id.clone(),
                caster_id: Some(row.caster_id.clone()),
                starts_at: Some(fmt_api_time(other_start)),
                ends_at: other_end.map(fmt_api_time),
                severity: "warning".to_string(),
            });
        };

        if !same_show {
            push(
                "overlap",
                format!(
                    "Überlappt mit Broadcast „{}“ ({} bis {})",
                    row.title,
                    fmt_api_time(other_start),
                    other_end
                        .map(fmt_api_time)
                        .unwrap_or_else(|| "offen".to_string())
                ),
            );
        }
        if slot.kind == BroadcastKind::Live && row.kind == BroadcastKind::Live {
            push(
                "live_overlap",
                format!(
                    "Live-Überlappung mit „{}“: nur ein Broadcast kann live sein (FCFS)",
                    row.title
                ),
            );
        }
        if row.caster_id == slot.caster_id {
            push(
                "caster_overlap",
                format!(
                    "Caster ist im selben Zeitraum bereits eingeplant („{}“)",
                    row.title
                ),
            );
        }
    }

    Ok(report)
}

#[derive(Debug, sqlx::FromRow)]
struct OverlapRow {
    id: String,
    show_id: Option<String>,
    caster_id: String,
    kind: BroadcastKind,
    title: String,
    starts_at: String,
    ends_at: Option<String>,
}
