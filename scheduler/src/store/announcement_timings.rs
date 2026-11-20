//! Announcement timings (Issue #36): the reminder lead time and the
//! `live_on`/`live_off` delays.
//!
//! Storage (plan §2): the **global** values live as a JSON object in
//! `settings['announcement.timings']` (a missing key means the §7 O11 defaults
//! `900 / 0 / 0`); a **per-show** override lives as one row in
//! `show_announcement_timings` (migration `0002`) where a `NULL` column means
//! "inherit the global value".
//!
//! [`resolve`] takes a [`SqliteConnection`] so it can run against a pool or an
//! open transaction — required by the broadcast hooks that plan reminders
//! inside their own transaction (avoiding the SQLite write lock, plan §8).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::dto::{
    AnnouncementTimingsDto, AnnouncementTimingsOverridesDto, ShowAnnouncementTimingsDto,
    UpdateAnnouncementTimingsRequest, UpdateShowAnnouncementTimingsRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::store::control;
use crate::store::now_ts;

/// `settings` key holding the global timings JSON object.
pub const SETTINGS_KEY: &str = "announcement.timings";

/// Largest accepted value for any timing (7 days, seconds); migration CHECK.
pub const MAX_SECONDS: i64 = 604_800;

/// Effective timings in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    pub lead: i64,
    pub live_on: i64,
    pub live_off: i64,
}

/// §7 O11 defaults: 15 min reminder, `live_on`/`live_off` immediate.
pub const DEFAULT: Timings = Timings {
    lead: 900,
    live_on: 0,
    live_off: 0,
};

/// Per-show overrides; `None` per field = inherit the global value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    pub lead: Option<i64>,
    pub live_on: Option<i64>,
    pub live_off: Option<i64>,
}

impl Overrides {
    /// True when every field is `NULL` (no effective override).
    pub fn is_empty(self) -> bool {
        self.lead.is_none() && self.live_on.is_none() && self.live_off.is_none()
    }
}

// ---------------------------------------------------------------------------
// Pure core (US-1)
// ---------------------------------------------------------------------------

/// Field-wins merge: a set override beats the global value per field.
pub fn merge(global: Timings, overrides: &Overrides) -> Timings {
    Timings {
        lead: overrides.lead.unwrap_or(global.lead),
        live_on: overrides.live_on.unwrap_or(global.live_on),
        live_off: overrides.live_off.unwrap_or(global.live_off),
    }
}

/// Validates one timing value (`0..=MAX_SECONDS`), else `422`.
pub fn validate(field: &'static str, value: i64) -> ApiResult<()> {
    if (0..=MAX_SECONDS).contains(&value) {
        Ok(())
    } else {
        Err(ApiError::validation(
            field,
            format!("{field} must be between 0 and {MAX_SECONDS}"),
        ))
    }
}

/// Reminder instant: `starts_at - lead`, clamped to `now` so a reminder is
/// never scheduled in the past (best effort, plan §5.2).
pub fn reminder_scheduled_at(
    starts_at: DateTime<Utc>,
    lead_seconds: i64,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let target = starts_at - Duration::seconds(lead_seconds);
    if target <= now { now } else { target }
}

/// Delay instant: `now + delay`; a delay of `0` yields `None` (= immediately
/// due, `scheduled_at = NULL`).
pub fn delay_scheduled_at(now: DateTime<Utc>, delay_seconds: i64) -> Option<DateTime<Utc>> {
    if delay_seconds == 0 {
        None
    } else {
        Some(now + Duration::seconds(delay_seconds))
    }
}

/// JSON shape persisted under [`SETTINGS_KEY`]; missing fields fall back to the
/// defaults (lenient read).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTimings {
    #[serde(default = "default_lead")]
    show_reminder_lead_seconds: i64,
    #[serde(default)]
    live_on_delay_seconds: i64,
    #[serde(default)]
    live_off_delay_seconds: i64,
}

fn default_lead() -> i64 {
    DEFAULT.lead
}

impl From<Timings> for StoredTimings {
    fn from(t: Timings) -> Self {
        StoredTimings {
            show_reminder_lead_seconds: t.lead,
            live_on_delay_seconds: t.live_on,
            live_off_delay_seconds: t.live_off,
        }
    }
}

impl From<StoredTimings> for Timings {
    fn from(s: StoredTimings) -> Self {
        Timings {
            lead: s.show_reminder_lead_seconds,
            live_on: s.live_on_delay_seconds,
            live_off: s.live_off_delay_seconds,
        }
    }
}

fn to_json(t: Timings) -> String {
    serde_json::to_string(&StoredTimings::from(t)).expect("timings serialize is infallible")
}

fn from_json(raw: &str) -> ApiResult<Timings> {
    let stored: StoredTimings = serde_json::from_str(raw).map_err(|err| {
        ApiError::Internal(anyhow::anyhow!("invalid stored {SETTINGS_KEY}: {err}"))
    })?;
    Ok(stored.into())
}

// ---------------------------------------------------------------------------
// Global settings (US-2)
// ---------------------------------------------------------------------------

/// Effective global timings; a missing key yields [`DEFAULT`].
pub async fn global(pool: &SqlitePool) -> ApiResult<Timings> {
    match control::get_setting(pool, SETTINGS_KEY).await? {
        Some(raw) => from_json(&raw),
        None => Ok(DEFAULT),
    }
}

/// Converts internal [`Timings`] into the wire DTO.
pub fn dto(timings: Timings) -> AnnouncementTimingsDto {
    AnnouncementTimingsDto {
        show_reminder_lead_seconds: timings.lead,
        live_on_delay_seconds: timings.live_on,
        live_off_delay_seconds: timings.live_off,
    }
}

/// Applies a partial global PATCH onto the current values (validated).
pub fn apply_patch(current: Timings, req: &UpdateAnnouncementTimingsRequest) -> ApiResult<Timings> {
    let mut next = current;
    if let Some(value) = req.show_reminder_lead_seconds {
        validate("show_reminder_lead_seconds", value)?;
        next.lead = value;
    }
    if let Some(value) = req.live_on_delay_seconds {
        validate("live_on_delay_seconds", value)?;
        next.live_on = value;
    }
    if let Some(value) = req.live_off_delay_seconds {
        validate("live_off_delay_seconds", value)?;
        next.live_off = value;
    }
    Ok(next)
}

/// Writes the fully validated global timings (always the complete object).
pub async fn set_global(pool: &SqlitePool, timings: Timings) -> ApiResult<Timings> {
    validate("show_reminder_lead_seconds", timings.lead)?;
    validate("live_on_delay_seconds", timings.live_on)?;
    validate("live_off_delay_seconds", timings.live_off)?;
    control::set_setting(pool, SETTINGS_KEY, &to_json(timings)).await?;
    Ok(timings)
}

/// Resolves the effective timings for `show_id`: global merged with the show
/// override (if any). Runs on any [`SqliteConnection`] (pool or `&mut *tx`).
pub async fn resolve(executor: &mut SqliteConnection, show_id: Option<&str>) -> ApiResult<Timings> {
    let global = match control::get_setting_conn(executor, SETTINGS_KEY).await? {
        Some(raw) => from_json(&raw)?,
        None => DEFAULT,
    };
    match show_id {
        Some(show_id) => {
            let overrides = load_overrides_conn(executor, show_id).await?;
            Ok(merge(global, &overrides))
        }
        None => Ok(global),
    }
}

// ---------------------------------------------------------------------------
// Per-show store (US-3)
// ---------------------------------------------------------------------------

/// Loads the override row for `show_id` (all `None` when absent).
async fn load_overrides_conn(conn: &mut SqliteConnection, show_id: &str) -> ApiResult<Overrides> {
    let row: Option<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT show_reminder_lead_seconds, live_on_delay_seconds, live_off_delay_seconds \
         FROM show_announcement_timings WHERE show_id = ?",
    )
    .bind(show_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        Some((lead, live_on, live_off)) => Overrides {
            lead,
            live_on,
            live_off,
        },
        None => Overrides::default(),
    })
}

/// Raw `show_announcement_timings` row: `(show_id, lead, live_on, live_off)`.
type OverrideRow = (String, Option<i64>, Option<i64>, Option<i64>);

/// Lists every show that overrides at least one timing.
pub async fn list(pool: &SqlitePool) -> ApiResult<Vec<ShowAnnouncementTimingsDto>> {
    let global = global(pool).await?;
    let rows: Vec<OverrideRow> = sqlx::query_as(
        "SELECT show_id, show_reminder_lead_seconds, live_on_delay_seconds, live_off_delay_seconds \
         FROM show_announcement_timings ORDER BY show_id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(show_id, lead, live_on, live_off)| {
            build_dto(
                &show_id,
                global,
                Overrides {
                    lead,
                    live_on,
                    live_off,
                },
            )
        })
        .collect())
}

/// `GET /announcement-timings/shows/{show_id}` — `404` for an unknown show.
pub async fn get(pool: &SqlitePool, show_id: &str) -> ApiResult<ShowAnnouncementTimingsDto> {
    ensure_show(pool, show_id).await?;
    let global = global(pool).await?;
    let overrides = load_overrides_pool(pool, show_id).await?;
    Ok(build_dto(show_id, global, overrides))
}

async fn load_overrides_pool(pool: &SqlitePool, show_id: &str) -> ApiResult<Overrides> {
    let mut conn = pool.acquire().await?;
    load_overrides_conn(&mut conn, show_id).await
}

/// `PUT /announcement-timings/shows/{show_id}` — upsert the show overrides.
///
/// Absent fields stay unchanged, `null` clears (inherit), a number overrides.
/// If every field ends up `NULL` the row is deleted (schema CHECK forbids an
/// all-`NULL` row).
pub async fn upsert(
    pool: &SqlitePool,
    show_id: &str,
    req: &UpdateShowAnnouncementTimingsRequest,
) -> ApiResult<ShowAnnouncementTimingsDto> {
    ensure_show(pool, show_id).await?;
    let global = global(pool).await?;
    let current = load_overrides_pool(pool, show_id).await?;
    let next = apply(current, req)?;

    if next.is_empty() {
        sqlx::query("DELETE FROM show_announcement_timings WHERE show_id = ?")
            .bind(show_id)
            .execute(pool)
            .await?;
    } else {
        let ts = now_ts();
        sqlx::query(
            "INSERT INTO show_announcement_timings \
             (show_id, show_reminder_lead_seconds, live_on_delay_seconds, live_off_delay_seconds, \
              created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(show_id) DO UPDATE SET \
               show_reminder_lead_seconds = excluded.show_reminder_lead_seconds, \
               live_on_delay_seconds = excluded.live_on_delay_seconds, \
               live_off_delay_seconds = excluded.live_off_delay_seconds, \
               updated_at = excluded.updated_at",
        )
        .bind(show_id)
        .bind(next.lead)
        .bind(next.live_on)
        .bind(next.live_off)
        .bind(&ts)
        .bind(&ts)
        .execute(pool)
        .await?;
    }
    Ok(build_dto(show_id, global, next))
}

/// `DELETE /announcement-timings/shows/{show_id}` — idempotent; `404` for an
/// unknown show.
pub async fn delete(pool: &SqlitePool, show_id: &str) -> ApiResult<()> {
    ensure_show(pool, show_id).await?;
    sqlx::query("DELETE FROM show_announcement_timings WHERE show_id = ?")
        .bind(show_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn apply(current: Overrides, req: &UpdateShowAnnouncementTimingsRequest) -> ApiResult<Overrides> {
    Ok(Overrides {
        lead: apply_field(
            "show_reminder_lead_seconds",
            current.lead,
            req.show_reminder_lead_seconds,
        )?,
        live_on: apply_field(
            "live_on_delay_seconds",
            current.live_on,
            req.live_on_delay_seconds,
        )?,
        live_off: apply_field(
            "live_off_delay_seconds",
            current.live_off,
            req.live_off_delay_seconds,
        )?,
    })
}

fn apply_field(
    field: &'static str,
    current: Option<i64>,
    incoming: Option<Option<i64>>,
) -> ApiResult<Option<i64>> {
    match incoming {
        None => Ok(current),
        Some(None) => Ok(None),
        Some(Some(value)) => {
            validate(field, value)?;
            Ok(Some(value))
        }
    }
}

fn build_dto(show_id: &str, global: Timings, overrides: Overrides) -> ShowAnnouncementTimingsDto {
    let effective = merge(global, &overrides);
    ShowAnnouncementTimingsDto {
        show_id: show_id.to_string(),
        effective: AnnouncementTimingsDto {
            show_reminder_lead_seconds: effective.lead,
            live_on_delay_seconds: effective.live_on,
            live_off_delay_seconds: effective.live_off,
        },
        overrides: AnnouncementTimingsOverridesDto {
            show_reminder_lead_seconds: overrides.lead,
            live_on_delay_seconds: overrides.live_on,
            live_off_delay_seconds: overrides.live_off,
        },
    }
}

async fn ensure_show(pool: &SqlitePool, show_id: &str) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shows WHERE id = ?")
        .bind(show_id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("show", show_id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn default_is_900_0_0() {
        assert_eq!(
            DEFAULT,
            Timings {
                lead: 900,
                live_on: 0,
                live_off: 0
            }
        );
    }

    #[test]
    fn merge_is_field_wise() {
        let global = Timings {
            lead: 900,
            live_on: 0,
            live_off: 0,
        };
        let only_lead = Overrides {
            lead: Some(300),
            ..Default::default()
        };
        assert_eq!(
            merge(global, &only_lead),
            Timings {
                lead: 300,
                live_on: 0,
                live_off: 0
            }
        );

        let all = Overrides {
            lead: Some(60),
            live_on: Some(5),
            live_off: Some(7),
        };
        assert_eq!(
            merge(global, &all),
            Timings {
                lead: 60,
                live_on: 5,
                live_off: 7
            }
        );

        assert_eq!(merge(global, &Overrides::default()), global);
    }

    #[test]
    fn validate_bounds() {
        assert!(validate("show_reminder_lead_seconds", 0).is_ok());
        assert!(validate("show_reminder_lead_seconds", MAX_SECONDS).is_ok());
        assert!(validate("show_reminder_lead_seconds", 604_800).is_ok());

        let low = validate("live_on_delay_seconds", -1).unwrap_err();
        assert!(matches!(
            low,
            ApiError::Validation {
                field: "live_on_delay_seconds",
                ..
            }
        ));
        let high = validate("live_off_delay_seconds", MAX_SECONDS + 1).unwrap_err();
        assert!(matches!(
            high,
            ApiError::Validation {
                field: "live_off_delay_seconds",
                ..
            }
        ));
    }

    #[test]
    fn json_roundtrip_and_defaults() {
        let t = Timings {
            lead: 600,
            live_on: 5,
            live_off: 0,
        };
        let raw = to_json(t);
        assert_eq!(from_json(&raw).unwrap(), t);

        // Missing fields fall back to defaults (lead ⇒ 900, others ⇒ 0).
        assert_eq!(from_json("{}").unwrap(), DEFAULT);
        assert_eq!(
            from_json(r#"{"live_on_delay_seconds": 42}"#).unwrap(),
            Timings {
                lead: 900,
                live_on: 42,
                live_off: 0
            }
        );

        // Corrupt JSON is an internal error, never a panic.
        assert!(from_json("not json").is_err());
    }

    #[test]
    fn reminder_due_and_clamp() {
        let start = utc(2026, 11, 20, 20, 0);
        let now = utc(2026, 11, 20, 10, 0);
        // start - 900s
        assert_eq!(
            reminder_scheduled_at(start, 900, now),
            utc(2026, 11, 20, 19, 45)
        );
        // start - 300s (override)
        assert_eq!(
            reminder_scheduled_at(start, 300, now),
            utc(2026, 11, 20, 19, 55)
        );
        // start in 10 min, lead 15 min ⇒ clamped to now
        let soon = utc(2026, 11, 20, 10, 10);
        assert_eq!(reminder_scheduled_at(soon, 900, now), now);
    }

    #[test]
    fn delay_zero_is_none() {
        let now = utc(2026, 11, 20, 10, 0);
        assert_eq!(delay_scheduled_at(now, 0), None);
        assert_eq!(delay_scheduled_at(now, 60), Some(utc(2026, 11, 20, 10, 1)));
    }
}
