//! Manual announcement handlers (Issue #38).
//!
//! Thin over the [`AnnouncementApi`]: RBAC is `announcement.write` for the
//! read and the post (portal.md §3.2). `caster_id` and the audit
//! actor always come from the session, never client input (§1). The scheduler
//! owns the authoritative RBAC + audit-log write.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::api_client::{AnnouncementError, NewAnnouncement};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::rbac;
use crate::templates::{AnnouncementRow, AnnouncementsPage};

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

fn announcement_error(err: AnnouncementError) -> PortalError {
    match err {
        AnnouncementError::NotFound => PortalError::NotFound,
        AnnouncementError::Validation { field, message } => {
            PortalError::Validation { field, message }
        }
        AnnouncementError::Conflict { message, .. } | AnnouncementError::Upstream(message) => {
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

/// `GET /announcements` — manual templates + the latest manual posts.
pub async fn announcements_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "announcement.write")?;
    let templates = state
        .announcements
        .list_templates(Some(&ctx.caster_id), Some("manual"))
        .await
        .map_err(announcement_error)?;
    let announcements = state
        .announcements
        .list(Some(&ctx.caster_id))
        .await
        .map_err(announcement_error)?;
    let mut recent: Vec<AnnouncementRow> = announcements
        .into_iter()
        .map(AnnouncementRow::from)
        .collect();
    // Newest first; the store orders ascending by creation.
    recent.reverse();
    render(&AnnouncementsPage {
        csrf_token: ctx.csrf_token.clone(),
        can_post: ctx.has("announcement.write"),
        caster_name: ctx.display_name.clone(),
        templates,
        recent,
    })
}

/// Body of `POST /announcements/post`.
#[derive(Debug, Deserialize)]
pub struct AnnouncementForm {
    pub caster_name: String,
    pub show_name: Option<String>,
}

/// `POST /announcements/post` — queue a manual announcement.
pub async fn announcement_post(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<AnnouncementForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "announcement.write")?;
    let req = NewAnnouncement {
        event: "manual".to_string(),
        // `caster_id` is the session identity, never the form (§1).
        caster_id: Some(ctx.caster_id.clone()),
        show_id: None,
        payload: serde_json::json!({
            "caster_name": form.caster_name,
            "show_name": blank_to_none(&form.show_name),
        }),
    };
    match state.announcements.create(Some(&ctx.caster_id), &req).await {
        Ok(_) => {
            if crate::queues::is_htmx(&headers) {
                Ok(crate::queues::refresh_with_toast(
                    "success",
                    "Announcement eingereiht.",
                ))
            } else {
                Ok(redirect("/announcements"))
            }
        }
        Err(AnnouncementError::Validation { field, message }) => {
            Err(PortalError::Validation { field, message })
        }
        Err(err) => Err(announcement_error(err)),
    }
}
