//! Show handlers (Issue #21, US-2).
//!
//! Thin over the [`ShowApi`]: RBAC is `show.write` for writes, `login` for
//! reads (portal.md §3.2). `caster_id`/`created_by` always come from the
//! session, never client input. Recurrence stays a raw rule string — an
//! invalid rule is a scheduler `422` rendered as an inline field error.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::api_client::{NewShow, ScheduleError, UpdateShow};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::rbac;
use crate::templates::{ShowDetailPage, ShowFormErrorFragment, ShowRow, ShowsPage};

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

fn blank_to_none(raw: &Option<String>) -> Option<String> {
    raw.as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Loads and orders the show list (own first, then by name).
async fn load_rows(
    state: &AppState,
    ctx: &AuthContext,
) -> Result<(Vec<ShowRow>, bool), PortalError> {
    let shows = state
        .shows
        .list(Some(&ctx.caster_id), None)
        .await
        .map_err(schedule_error)?;
    let can_write = ctx.has("show.write");
    let mut rows: Vec<ShowRow> = shows
        .into_iter()
        .map(|show| ShowRow {
            is_own: show.caster_id.as_deref() == Some(ctx.caster_id.as_str()),
            can_write,
            has_recurrence: show.recurrence.is_some(),
            show,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.is_own
            .cmp(&a.is_own)
            .then_with(|| a.show.name.cmp(&b.show.name))
    });
    Ok((rows, can_write))
}

/// `GET /shows` — own + foreign shows (read-only) + create form.
pub async fn shows_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    let (shows, can_write) = load_rows(&state, &ctx).await?;
    render(&ShowsPage {
        csrf_token: ctx.csrf_token.clone(),
        can_write,
        shows,
    })
}

/// `GET /shows/{id}` — show detail + edit form.
pub async fn show_detail(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Response, PortalError> {
    let show = state
        .shows
        .get(Some(&ctx.caster_id), &id)
        .await
        .map_err(schedule_error)?;
    let has_recurrence = show.recurrence.is_some();
    let recurrence_text = show.recurrence.clone().unwrap_or_default();
    render(&ShowDetailPage {
        csrf_token: ctx.csrf_token.clone(),
        show,
        can_write: ctx.has("show.write"),
        has_recurrence,
        recurrence_text,
    })
}

/// Body of `POST /shows`.
#[derive(Debug, Deserialize)]
pub struct ShowForm {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub recurrence: Option<String>,
}

/// `POST /shows` — create a show.
pub async fn show_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<ShowForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "show.write")?;
    let req = NewShow {
        slug: form.slug.clone(),
        name: form.name.clone(),
        description: blank_to_none(&form.description),
        artwork_url: None,
        recurrence: blank_to_none(&form.recurrence),
        caster_id: Some(ctx.caster_id.clone()),
        created_by: ctx.caster_id.clone(),
    };
    match state.shows.create(Some(&ctx.caster_id), &req).await {
        Ok(_) => {
            if crate::queues::is_htmx(&headers) {
                Ok(crate::queues::refresh_with_toast(
                    "success",
                    "Show angelegt.",
                ))
            } else {
                Ok(redirect("/shows"))
            }
        }
        Err(ScheduleError::Validation { message, .. }) => {
            if crate::queues::is_htmx(&headers) {
                render(&ShowFormErrorFragment {
                    error: Some(message),
                })
            } else {
                Err(PortalError::Validation {
                    field: "recurrence".into(),
                    message,
                })
            }
        }
        Err(err) => Err(schedule_error(err)),
    }
}

/// `POST /shows/{id}` — update a show.
pub async fn show_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<ShowForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "show.write")?;
    let req = UpdateShow {
        slug: Some(form.slug.clone()),
        name: Some(form.name.clone()),
        description: blank_to_none(&form.description),
        recurrence: blank_to_none(&form.recurrence),
        caster_id: None,
    };
    match state.shows.update(Some(&ctx.caster_id), &id, &req).await {
        Ok(_) => {
            if crate::queues::is_htmx(&headers) {
                Ok(crate::queues::refresh_with_toast(
                    "success",
                    "Show gespeichert.",
                ))
            } else {
                Ok(redirect(&format!("/shows/{id}")))
            }
        }
        Err(ScheduleError::Validation { message, .. }) => {
            if crate::queues::is_htmx(&headers) {
                render(&ShowFormErrorFragment {
                    error: Some(message),
                })
            } else {
                Err(PortalError::Validation {
                    field: "recurrence".into(),
                    message,
                })
            }
        }
        Err(err) => Err(schedule_error(err)),
    }
}
