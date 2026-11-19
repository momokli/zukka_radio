//! Schedule/calendar handlers (Issue #21).
//!
//! The portal is thin (portal.md §1.3): it computes the visible window on its
//! own (the `/schedule` endpoint returns entries, not navigation metadata) but
//! **never** decides conflicts — it renders the scheduler's `warnings` (D12).
//! `caster_id`/`created_by` always come from the session, never client input.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use chrono::Datelike;
use serde::Deserialize;

use crate::api_client::{Broadcast, NewBroadcast, ScheduleEntry, ScheduleError, UpdateBroadcast};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::rbac;
use crate::templates::{
    BroadcastCard, BroadcastForm, BroadcastFormFragment, BroadcastFormPage, CalendarCell,
    CalendarGridFragment, CalendarWeek, QueueOption, SchedulePage, ShowOption, WarningView,
};
use crate::tz::{self, CalendarView};

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

fn schedule_error(err: ScheduleError) -> PortalError {
    match err {
        ScheduleError::NotFound => PortalError::NotFound,
        ScheduleError::Validation { field, message } => PortalError::Validation { field, message },
        ScheduleError::Conflict { message, .. } | ScheduleError::Upstream(message) => {
            PortalError::Upstream(message)
        }
    }
}

// ---------------------------------------------------------------------------
// US-3 / US-4: calendar page + grid fragment
// ---------------------------------------------------------------------------

/// Query of `GET /schedule`.
#[derive(Debug, Deserialize)]
pub struct ScheduleQuery {
    /// `week` (default) | `month`.
    pub view: Option<String>,
    /// `YYYY-MM-DD` Berlin anchor date.
    pub anchor: Option<String>,
    /// `calendar-grid` forces the fragment (htmx uses `HX-Request`).
    pub fragment: Option<String>,
}

fn parse_anchor(raw: Option<&str>) -> Option<chrono::NaiveDate> {
    let raw = raw?.trim();
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()
}

/// Builds the week-major grid from the scheduler entries.
fn build_grid(
    view: CalendarView,
    anchor: chrono::NaiveDate,
    entries: Vec<ScheduleEntry>,
) -> Vec<CalendarWeek> {
    let days = view.grid(anchor);
    let today = tz::today_berlin();
    let first = *days.first().expect("non-empty grid");

    // Bucket entries by their Berlin start day, clamped into the grid.
    let mut buckets: Vec<Vec<BroadcastCard>> = vec![Vec::new(); days.len()];
    for entry in entries {
        let Some(start) = tz::parse_api(&entry.starts_at) else {
            continue;
        };
        let start_day = tz::to_berlin(start).date_naive();
        let idx = if start_day < first {
            0
        } else {
            match days.iter().position(|d| *d == start_day) {
                Some(i) => i,
                None => continue, // outside the visible grid
            }
        };
        let open_end = entry.ends_at.is_none();
        let time_label = match entry.ends_at.as_deref().and_then(tz::parse_api) {
            Some(end) => format!("{}–{}", tz::fmt_hm(start), tz::fmt_hm(end)),
            None => format!("{} →", tz::fmt_hm(start)),
        };
        buckets[idx].push(BroadcastCard {
            id: entry.id.clone().unwrap_or_default(),
            persisted: entry.persisted,
            title: entry.title,
            kind: entry.kind,
            status: entry.status,
            caster_id: entry.caster_id,
            time_label,
            open_end,
            warnings: entry.warnings.into_iter().map(warning_view).collect(),
        });
    }

    days.into_iter()
        .zip(buckets)
        .collect::<Vec<_>>()
        .chunks(7)
        .map(|chunk| CalendarWeek {
            cells: chunk
                .iter()
                .map(|(date, entries)| CalendarCell {
                    date: date.to_string(),
                    day_code: tz::weekday_code(date.weekday()).to_string(),
                    day_num: format!("{:02}", chrono::Datelike::day(date)),
                    in_month: view != CalendarView::Month
                        || (date.month() == anchor.month() && date.year() == anchor.year()),
                    is_today: *date == today,
                    entries: entries.clone(),
                })
                .collect(),
        })
        .collect()
}

fn warning_view(w: crate::api_client::Warning) -> WarningView {
    WarningView {
        code: w.code,
        message: w.message,
        severity: if w.severity.is_empty() {
            "warning".into()
        } else {
            w.severity
        },
    }
}

/// `GET /schedule` — calendar (page, or `calendar-grid` fragment for htmx).
pub async fn schedule_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<ScheduleQuery>,
    headers: HeaderMap,
) -> Result<Response, PortalError> {
    let view = CalendarView::parse(q.view.as_deref());
    let anchor = parse_anchor(q.anchor.as_deref()).unwrap_or_else(tz::today_berlin);
    let (from, to) = view.range(anchor);
    let entries = state
        .schedule
        .window(Some(&ctx.caster_id), &tz::fmt_utc(from), &tz::fmt_utc(to))
        .await
        .map_err(schedule_error)?;
    let weeks = build_grid(view, anchor, entries);
    let can_write = ctx.has("broadcast.write");
    let fragment =
        crate::queues::is_htmx(&headers) || q.fragment.as_deref() == Some("calendar-grid");
    if fragment {
        render(&CalendarGridFragment {
            csrf_token: ctx.csrf_token.clone(),
            view: view.as_str().into(),
            view_label: view.label().into(),
            anchor: anchor.to_string(),
            prev_anchor: view.shift(anchor, -1).to_string(),
            next_anchor: view.shift(anchor, 1).to_string(),
            title: view.title(anchor),
            weeks,
            can_write,
        })
    } else {
        render(&SchedulePage {
            csrf_token: ctx.csrf_token.clone(),
            view: view.as_str().into(),
            view_label: view.label().into(),
            anchor: anchor.to_string(),
            prev_anchor: view.shift(anchor, -1).to_string(),
            next_anchor: view.shift(anchor, 1).to_string(),
            title: view.title(anchor),
            weeks,
            can_write,
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// US-5: broadcast form (Berlin input, UTC wire)
// ---------------------------------------------------------------------------

/// Query of `GET /schedule/new` (also used for the date-change preview, so it
/// carries the whole in-flight form to preserve its values).
#[derive(Debug, Deserialize)]
pub struct BroadcastFormQuery {
    pub id: Option<String>,
    pub version: Option<i64>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: Option<String>,
    pub show_id: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub tape_id: Option<String>,
    pub queue_id: Option<String>,
}

/// Body of `POST /schedule` / `POST /schedule/{id}`.
#[derive(Debug, Deserialize)]
pub struct BroadcastFormPost {
    pub title: String,
    pub description: Option<String>,
    pub kind: String,
    pub show_id: Option<String>,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub tape_id: Option<String>,
    pub queue_id: Option<String>,
    pub version: Option<i64>,
}

fn blank_to_none(raw: &Option<String>) -> Option<String> {
    raw.as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parses a required Berlin `datetime-local` value into an RFC3339 UTC string.
fn parse_required_local(field: &str, raw: &str) -> Result<String, PortalError> {
    let naive = tz::parse_input(raw).ok_or_else(|| PortalError::Validation {
        field: field.into(),
        message: format!("{field} ist erforderlich (Berliner Zeit)"),
    })?;
    Ok(tz::fmt_utc(tz::berlin_to_utc(naive, false).0))
}

/// Parses an optional Berlin `datetime-local` value into an RFC3339 UTC string.
fn parse_optional_local(field: &str, raw: &Option<String>) -> Result<Option<String>, PortalError> {
    match blank_to_none(raw) {
        None => Ok(None),
        Some(raw) => {
            let naive = tz::parse_input(&raw).ok_or_else(|| PortalError::Validation {
                field: field.into(),
                message: format!("{field} ist ungültig (Berliner Zeit)"),
            })?;
            Ok(Some(tz::fmt_utc(tz::berlin_to_utc(naive, true).0)))
        }
    }
}

/// Loads the show/queue select options.
async fn form_options(
    state: &AppState,
    ctx: &AuthContext,
) -> Result<(Vec<ShowOption>, Vec<QueueOption>), PortalError> {
    let shows = state
        .shows
        .list(Some(&ctx.caster_id), None)
        .await
        .map_err(schedule_error)?;
    let show_options = shows
        .into_iter()
        .map(|s| ShowOption {
            id: s.id,
            name: s.name,
        })
        .collect();
    let queue_options = state
        .queues
        .list(
            Some(&ctx.caster_id),
            &crate::api_client::QueueQuery::default(),
        )
        .await
        .map(|queues| {
            queues
                .into_iter()
                .map(|q| QueueOption {
                    id: q.id,
                    name: q.name,
                })
                .collect()
        })
        .unwrap_or_else(|err| {
            // The queue select is a convenience; the scheduler still validates
            // `queue_id`. Degrade to an empty list instead of failing the form.
            tracing::warn!("queue list unavailable for schedule form: {err}");
            Vec::new()
        });
    Ok((show_options, queue_options))
}

/// Empty form for `GET /schedule/new`.
async fn empty_form(state: &AppState, ctx: &AuthContext) -> Result<BroadcastForm, PortalError> {
    let (shows, queues) = form_options(state, ctx).await?;
    Ok(BroadcastForm {
        id: String::new(),
        version: 0,
        title: String::new(),
        description: String::new(),
        kind: "live".into(),
        show_id: String::new(),
        starts_at: String::new(),
        ends_at: String::new(),
        tape_id: String::new(),
        queue_id: String::new(),
        shows,
        queues,
    })
}

/// Form filled from an existing broadcast (`GET /schedule/{id}/edit`).
async fn form_from_broadcast(
    state: &AppState,
    ctx: &AuthContext,
    broadcast: &Broadcast,
) -> Result<BroadcastForm, PortalError> {
    let (shows, queues) = form_options(state, ctx).await?;
    Ok(BroadcastForm {
        id: broadcast.id.clone(),
        version: broadcast.version,
        title: broadcast.title.clone(),
        description: broadcast.description.clone(),
        kind: broadcast.kind.clone(),
        show_id: broadcast.show_id.clone().unwrap_or_default(),
        starts_at: tz::parse_api(&broadcast.starts_at)
            .map(tz::fmt_input)
            .unwrap_or_default(),
        ends_at: broadcast
            .ends_at
            .as_deref()
            .and_then(tz::parse_api)
            .map(tz::fmt_input)
            .unwrap_or_default(),
        tape_id: broadcast.tape_id.clone().unwrap_or_default(),
        queue_id: broadcast.queue_id.clone().unwrap_or_default(),
        shows,
        queues,
    })
}

/// Soft-conflict preview: the scheduler's warnings for the entered window.
///
/// The portal does **not** compute conflicts — it asks the scheduler for the
/// window and renders whatever warnings come back (D12), excluding the entry
/// being edited.
async fn preview_warnings(
    state: &AppState,
    ctx: &AuthContext,
    form: &BroadcastForm,
) -> Vec<WarningView> {
    let Some(naive) = tz::parse_input(&form.starts_at) else {
        return Vec::new();
    };
    let from = tz::berlin_to_utc(naive, false).0;
    let to = tz::parse_input(&form.ends_at)
        .map(|n| tz::berlin_to_utc(n, true).0)
        .unwrap_or(from);
    let Ok(entries) = state
        .schedule
        .window(Some(&ctx.caster_id), &tz::fmt_utc(from), &tz::fmt_utc(to))
        .await
    else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for entry in entries {
        if entry.id.as_deref() == Some(form.id.as_str()) && !form.id.is_empty() {
            continue;
        }
        for w in entry.warnings {
            let key = (w.code.clone(), w.broadcast_id.clone().unwrap_or_default());
            if seen.insert(key) {
                out.push(warning_view(w));
            }
        }
    }
    out
}

async fn render_form_fragment(
    _state: &AppState,
    ctx: &AuthContext,
    form: BroadcastForm,
    warnings: Vec<WarningView>,
    error: Option<String>,
    toast: Option<(&str, &str)>,
) -> Result<Response, PortalError> {
    let mut response = render(&BroadcastFormFragment {
        csrf_token: ctx.csrf_token.clone(),
        form,
        warnings,
        error,
    })?;
    if let Some((level, message)) = toast {
        crate::queues::set_toast(&mut response, level, message);
    }
    Ok(response)
}

/// `GET /schedule/new` — broadcast create form (or `broadcast-form` fragment
/// after a date change, showing inline warnings).
pub async fn broadcast_new(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<BroadcastFormQuery>,
    headers: HeaderMap,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "broadcast.write")?;
    let mut form = empty_form(&state, &ctx).await?;
    if let Some(id) = &q.id {
        form.id = id.clone();
    }
    if let Some(version) = q.version {
        form.version = version;
    }
    if let Some(title) = &q.title {
        form.title = title.clone();
    }
    if let Some(description) = &q.description {
        form.description = description.clone();
    }
    if let Some(kind) = &q.kind {
        form.kind = kind.clone();
    }
    if let Some(show_id) = &q.show_id {
        form.show_id = show_id.clone();
    }
    if let Some(starts_at) = &q.starts_at {
        form.starts_at = starts_at.clone();
    }
    if let Some(ends_at) = &q.ends_at {
        form.ends_at = ends_at.clone();
    }
    if let Some(tape_id) = &q.tape_id {
        form.tape_id = tape_id.clone();
    }
    if let Some(queue_id) = &q.queue_id {
        form.queue_id = queue_id.clone();
    }
    let warnings = preview_warnings(&state, &ctx, &form).await;
    if crate::queues::is_htmx(&headers) {
        render_form_fragment(&state, &ctx, form, warnings, None, None).await
    } else {
        render(&BroadcastFormPage {
            csrf_token: ctx.csrf_token.clone(),
            form,
            warnings,
            error: None,
        })
    }
}

/// `GET /schedule/{id}/edit` — broadcast edit form.
pub async fn broadcast_edit_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "broadcast.write")?;
    let broadcast = state
        .broadcasts
        .get(Some(&ctx.caster_id), &id)
        .await
        .map_err(schedule_error)?;
    let form = form_from_broadcast(&state, &ctx, &broadcast).await?;
    let warnings = broadcast.warnings.into_iter().map(warning_view).collect();
    render(&BroadcastFormPage {
        csrf_token: ctx.csrf_token.clone(),
        form,
        warnings,
        error: None,
    })
}

/// `POST /schedule` — create a broadcast.
pub async fn broadcast_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<BroadcastFormPost>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "broadcast.write")?;
    let starts_at = parse_required_local("starts_at", &form.starts_at)?;
    let ends_at = parse_optional_local("ends_at", &form.ends_at)?;
    let req = NewBroadcast {
        caster_id: ctx.caster_id.clone(),
        show_id: blank_to_none(&form.show_id),
        kind: form.kind.clone(),
        title: form.title.clone(),
        description: blank_to_none(&form.description),
        starts_at,
        ends_at,
        tape_id: blank_to_none(&form.tape_id),
        queue_id: blank_to_none(&form.queue_id),
        created_by: Some(ctx.caster_id.clone()),
    };
    match state.broadcasts.create(Some(&ctx.caster_id), &req).await {
        Ok(_) => {
            if crate::queues::is_htmx(&headers) {
                Ok(crate::queues::refresh_with_toast(
                    "success",
                    "Broadcast angelegt.",
                ))
            } else {
                Ok(redirect("/schedule"))
            }
        }
        Err(err @ (ScheduleError::Validation { .. } | ScheduleError::Conflict { .. })) => {
            form_error_response(&state, &ctx, &headers, None, &form, err).await
        }
        Err(err) => Err(schedule_error(err)),
    }
}

/// `POST /schedule/{id}` — update a broadcast (optimistic `version`).
pub async fn broadcast_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<BroadcastFormPost>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "broadcast.write")?;
    let starts_at = parse_required_local("starts_at", &form.starts_at)?;
    let ends_at = parse_optional_local("ends_at", &form.ends_at)?;
    let version = form.version.unwrap_or(0);
    let req = UpdateBroadcast {
        kind: Some(form.kind.clone()),
        title: Some(form.title.clone()),
        description: blank_to_none(&form.description),
        starts_at: Some(starts_at),
        ends_at: Some(ends_at),
        show_id: Some(blank_to_none(&form.show_id)),
        tape_id: Some(blank_to_none(&form.tape_id)),
        queue_id: Some(blank_to_none(&form.queue_id)),
        version,
    };
    match state
        .broadcasts
        .update(Some(&ctx.caster_id), &id, &req)
        .await
    {
        Ok(_) => {
            if crate::queues::is_htmx(&headers) {
                Ok(crate::queues::refresh_with_toast(
                    "success",
                    "Broadcast gespeichert.",
                ))
            } else {
                Ok(redirect("/schedule"))
            }
        }
        Err(err @ (ScheduleError::Validation { .. } | ScheduleError::Conflict { .. })) => {
            form_error_response(&state, &ctx, &headers, Some(&id), &form, err).await
        }
        Err(err) => Err(schedule_error(err)),
    }
}

/// A `422`/`409` from create/update: htmx re-renders the form inline with the
/// entered values (a `409 show_overlap` additionally raises a toast) — the UI
/// never becomes leading. Non-htmx returns the field error.
///
/// `id` is the URL `Path` id for an update (`None` for create); it must be kept
/// so the re-rendered fragment posts back to `/schedule/{id}` with its `version`
/// hidden field — otherwise a corrected save would create a *new* broadcast.
async fn form_error_response(
    state: &AppState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Option<&str>,
    form: &BroadcastFormPost,
    err: ScheduleError,
) -> Result<Response, PortalError> {
    if !crate::queues::is_htmx(headers) {
        return Err(schedule_error(err));
    }
    let mut filled = empty_form(state, ctx).await?;
    if let Some(id) = id {
        filled.id = id.to_string();
        filled.version = form.version.unwrap_or(0);
    }
    filled.title = form.title.clone();
    filled.description = form.description.clone().unwrap_or_default();
    filled.kind = form.kind.clone();
    filled.show_id = form.show_id.clone().unwrap_or_default();
    filled.starts_at = form.starts_at.clone();
    filled.ends_at = form.ends_at.clone().unwrap_or_default();
    filled.tape_id = form.tape_id.clone().unwrap_or_default();
    filled.queue_id = form.queue_id.clone().unwrap_or_default();
    let warnings = preview_warnings(state, ctx, &filled).await;
    match err {
        ScheduleError::Validation { message, .. } => {
            render_form_fragment(state, ctx, filled, warnings, Some(message), None).await
        }
        ScheduleError::Conflict { code, message } => {
            let text = if code == "show_overlap" {
                "Diese Show überlappt sich selbst in diesem Zeitraum — bitte anpassen.".to_string()
            } else {
                format!("Konflikt: {message}")
            };
            render_form_fragment(
                state,
                ctx,
                filled,
                warnings,
                Some(text),
                Some(("error", "Nicht gespeichert.")),
            )
            .await
        }
        other => Err(schedule_error(other)),
    }
}

/// `POST /schedule/{id}/delete` — delete a broadcast.
pub async fn broadcast_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "broadcast.write")?;
    match state.broadcasts.delete(Some(&ctx.caster_id), &id).await {
        Ok(()) => {
            if crate::queues::is_htmx(&headers) {
                let mut response = StatusCode::NO_CONTENT.into_response();
                response.headers_mut().insert(
                    header::HeaderName::from_static("hx-redirect"),
                    header::HeaderValue::from_static("/schedule"),
                );
                crate::queues::set_toast(&mut response, "success", "Broadcast gelöscht.");
                Ok(response)
            } else {
                Ok(redirect("/schedule"))
            }
        }
        Err(err) => Err(schedule_error(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, starts: &str, ends: Option<&str>, title: &str) -> ScheduleEntry {
        ScheduleEntry {
            id: Some(id.into()),
            show_id: None,
            caster_id: "c-1".into(),
            kind: "live".into(),
            title: title.into(),
            starts_at: starts.into(),
            ends_at: ends.map(str::to_string),
            status: "scheduled".into(),
            persisted: true,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn grid_buckets_entries_by_berlin_day() {
        // 2026-11-18 20:00+01:00 = 19:00Z, a Wednesday.
        let entries = vec![entry(
            "b1",
            "2026-11-18T20:00:00+01:00",
            Some("2026-11-18T22:00:00+01:00"),
            "Late Show",
        )];
        let weeks = build_grid(
            CalendarView::Week,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
            entries,
        );
        let cell = weeks[0]
            .cells
            .iter()
            .find(|c| c.date == "2026-11-18")
            .expect("wednesday cell");
        assert_eq!(cell.entries.len(), 1);
        assert_eq!(cell.entries[0].time_label, "20:00–22:00");
        assert!(!cell.entries[0].open_end);
    }

    #[test]
    fn open_ended_entry_renders_open_bar() {
        let entries = vec![entry("b1", "2026-11-18T20:00:00+01:00", None, "Open")];
        let weeks = build_grid(
            CalendarView::Week,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
            entries,
        );
        let cell = &weeks[0].cells[2];
        assert!(cell.entries[0].open_end);
        assert!(cell.entries[0].time_label.ends_with('→'));
    }

    #[test]
    fn entry_before_window_is_clamped_into_first_cell() {
        // Starts Sunday 23:00 the day before the Monday window.
        let entries = vec![entry("b1", "2026-11-15T23:00:00+01:00", None, "Overnight")];
        let weeks = build_grid(
            CalendarView::Week,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
            entries,
        );
        assert_eq!(weeks[0].cells[0].entries.len(), 1, "clamped to Monday");
    }

    #[test]
    fn required_local_rejects_garbage() {
        let err = parse_required_local("starts_at", "not-a-time").expect_err("must fail");
        assert!(matches!(err, PortalError::Validation { ref field, .. } if field == "starts_at"));
    }

    #[test]
    fn optional_local_blank_is_none() {
        assert_eq!(parse_optional_local("ends_at", &None).unwrap(), None);
        assert_eq!(
            parse_optional_local("ends_at", &Some("  ".into())).unwrap(),
            None
        );
    }
}
