//! Schedule store: conflict detection (§4), window merge and expansion.
//!
//! This module owns the shared overlap rules; the broadcast store calls
//! [`detect_conflicts`] for hard self-overlaps and soft warnings.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use crate::dto::{BroadcastKind, BroadcastStatus, ExpandResultDto, ScheduleEntryDto, WarningDto};
use crate::error::{ApiError, ApiResult};
use crate::rrule::RecurrenceRule;
use crate::store::now_ts;
use crate::time::{fmt_api_time, fmt_storage_time, parse_api_time, parse_storage_time};
use crate::uuid::new_id;

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

/// One show row with a recurrence rule, used by merge and expansion.
#[derive(Debug, sqlx::FromRow)]
struct ShowRecurrenceRow {
    id: String,
    name: String,
    description: String,
    caster_id: Option<String>,
    created_by: String,
    recurrence: String,
}

async fn shows_with_recurrence(pool: &SqlitePool) -> ApiResult<Vec<ShowRecurrenceRow>> {
    let rows = sqlx::query_as(
        "SELECT id, name, description, caster_id, created_by, recurrence FROM shows \
         WHERE recurrence IS NOT NULL ORDER BY created_at, id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

fn parse_stored_rule(row: &ShowRecurrenceRow) -> ApiResult<RecurrenceRule> {
    RecurrenceRule::parse("recurrence", &row.recurrence).map_err(|err| {
        ApiError::Internal(anyhow::anyhow!(
            "invalid stored recurrence for show {}: {err}",
            row.id
        ))
    })
}

/// Merged window view: persisted broadcasts plus computed occurrences (§1.3).
///
/// Computed occurrences that already exist as a broadcast for the same show
/// and start are not listed twice (plan R9).
pub async fn merged_window(
    pool: &SqlitePool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> ApiResult<Vec<ScheduleEntryDto>> {
    let mut entries: Vec<(DateTime<Utc>, ScheduleEntryDto)> = Vec::new();
    let mut persisted_keys: HashSet<(String, String)> = HashSet::new();
    let mut conn = pool.acquire().await?;

    let persisted = crate::store::broadcasts::list(pool, Some(from), Some(to), None, None).await?;
    for dto in persisted {
        if dto.status == BroadcastStatus::Cancelled {
            continue;
        }
        let start = parse_api_time("starts_at", &dto.starts_at)?;
        let end = match &dto.ends_at {
            Some(raw) => Some(parse_api_time("ends_at", raw)?),
            None => None,
        };
        if let Some(show_id) = &dto.show_id {
            persisted_keys.insert((show_id.clone(), fmt_storage_time(start)));
        }

        let warnings = {
            let slot = Slot {
                id: Some(&dto.id),
                show_id: dto.show_id.as_deref(),
                caster_id: &dto.caster_id,
                kind: dto.kind,
                starts_at: start,
                ends_at: end,
            };
            detect_conflicts(&mut conn, &slot).await?.warnings
        };

        entries.push((
            start,
            ScheduleEntryDto {
                id: Some(dto.id),
                show_id: dto.show_id,
                caster_id: dto.caster_id,
                kind: dto.kind,
                title: dto.title,
                starts_at: dto.starts_at,
                ends_at: dto.ends_at,
                status: dto.status,
                persisted: true,
                warnings,
            },
        ));
    }

    for show in shows_with_recurrence(pool).await? {
        let rule = parse_stored_rule(&show)?;
        let caster_id = show
            .caster_id
            .clone()
            .unwrap_or_else(|| show.created_by.clone());
        for occurrence in rule.occurrences(from, to) {
            if persisted_keys.contains(&(show.id.clone(), fmt_storage_time(occurrence.start_utc))) {
                continue;
            }
            let warnings = {
                let slot = Slot {
                    id: None,
                    show_id: Some(&show.id),
                    caster_id: &caster_id,
                    kind: BroadcastKind::Live,
                    starts_at: occurrence.start_utc,
                    ends_at: occurrence.end_utc,
                };
                detect_conflicts(&mut conn, &slot).await?.warnings
            };
            entries.push((
                occurrence.start_utc,
                ScheduleEntryDto {
                    id: None,
                    show_id: Some(show.id.clone()),
                    caster_id: caster_id.clone(),
                    kind: BroadcastKind::Live,
                    title: show.name.clone(),
                    starts_at: fmt_api_time(occurrence.start_utc),
                    ends_at: occurrence.end_utc.map(fmt_api_time),
                    status: BroadcastStatus::Scheduled,
                    persisted: false,
                    warnings,
                },
            ));
        }
    }

    entries.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            let a_key = a.1.id.clone().unwrap_or_default();
            let b_key = b.1.id.clone().unwrap_or_default();
            a_key.cmp(&b_key)
        })
    });
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

/// Persists all recurrence occurrences in the window (§5, idempotent).
///
/// Lookup-in-transaction on `(show_id, starts_at, status != 'cancelled')`:
/// there is no unique index for this pair, so a repeated call skips existing
/// rows instead of inserting duplicates.
pub async fn expand(
    pool: &SqlitePool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> ApiResult<ExpandResultDto> {
    let shows = shows_with_recurrence(pool).await?;
    let mut planned = Vec::new();
    for show in &shows {
        let rule = parse_stored_rule(show)?;
        let caster_id = show
            .caster_id
            .clone()
            .unwrap_or_else(|| show.created_by.clone());
        for occurrence in rule.occurrences(from, to) {
            planned.push((show, occurrence, caster_id.clone()));
        }
    }

    let mut tx = pool.begin().await?;
    let mut created = 0i64;
    let mut skipped = 0i64;
    let mut warnings = Vec::new();

    for (show, occurrence, caster_id) in &planned {
        let starts_storage = fmt_storage_time(occurrence.start_utc);
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM broadcasts \
             WHERE show_id = ? AND starts_at = ? AND status != 'cancelled'",
        )
        .bind(&show.id)
        .bind(&starts_storage)
        .fetch_optional(&mut *tx)
        .await?;
        if existing.is_some() {
            skipped += 1;
            continue;
        }

        let report = detect_conflicts(
            &mut tx,
            &Slot {
                id: None,
                show_id: Some(&show.id),
                caster_id,
                kind: BroadcastKind::Live,
                starts_at: occurrence.start_utc,
                ends_at: occurrence.end_utc,
            },
        )
        .await?;
        warnings.extend(report.warnings);

        let id = new_id();
        let ts = now_ts();
        let ends_storage = occurrence.end_utc.map(fmt_storage_time);
        sqlx::query(
            "INSERT INTO broadcasts \
             (id, show_id, caster_id, kind, title, description, starts_at, ends_at, status, \
              tape_id, queue_id, version, created_by, created_at, updated_at) \
             VALUES (?, ?, ?, 'live', ?, ?, ?, ?, 'scheduled', NULL, NULL, 1, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&show.id)
        .bind(caster_id)
        .bind(&show.name)
        .bind(&show.description)
        .bind(&starts_storage)
        .bind(ends_storage.as_deref())
        .bind(caster_id)
        .bind(&ts)
        .bind(&ts)
        .execute(&mut *tx)
        .await?;

        crate::store::broadcasts::insert_event_log(
            &mut *tx,
            "broadcast_created",
            Some(caster_id),
            Some(&id),
            Some(caster_id),
            serde_json::json!({
                "version": 1,
                "kind": "live",
                "source": "schedule_expand",
                "show_id": show.id,
            }),
        )
        .await?;

        if occurrence.dst_adjusted {
            crate::store::broadcasts::insert_event_log(
                &mut *tx,
                "dst_adjusted",
                Some(caster_id),
                Some(&id),
                Some(caster_id),
                serde_json::json!({
                    "show_id": show.id,
                    "starts_at": fmt_api_time(occurrence.start_utc),
                }),
            )
            .await?;
        }

        created += 1;
    }

    tx.commit().await?;

    Ok(ExpandResultDto {
        from: fmt_api_time(from),
        to: fmt_api_time(to),
        created,
        skipped,
        warnings,
    })
}
