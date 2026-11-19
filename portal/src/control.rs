//! Control-panel handlers (Issue #22, US-3…US-6).
//!
//! Thin UI over the scheduler's control API: `GET /control` renders the panel,
//! `GET /control/state` returns the `control-state` fragment (SSE `state` +
//! 3 s polling fallback), `GET /control/events` is the SSE stream and
//! `GET /control/badge` the per-viewer live indicator. Writes go through
//! `POST /control/source|pause|skip` and are gated on `control.write`
//! (§6.10.1); `caster` therefore gets `403`. Every read needs `control.read`.
//!
//! A scheduler outage never becomes a `500`: the fragment renders a hint (D20).

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::Extension;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::IntervalStream;

use crate::api_client::{ControlError, ControlState};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::queues::{ascii_json, is_htmx, toast_only};
use crate::rbac;
use crate::templates::{
    ControlPage, ControlStateFragment, ControlView, LiveBadgeFragment, LiveBadgeView,
};

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

/// Loads the control view, degrading to a hint on an unreachable scheduler (D20).
async fn load_view(state: &AppState, ctx: &AuthContext) -> ControlView {
    let can_write = ctx.has("control.write");
    match state.control.state(Some(&ctx.caster_id)).await {
        Ok(snapshot) => ControlView::from_state(snapshot, can_write),
        Err(err) => {
            tracing::warn!("control state unavailable: {err}");
            ControlView::unreachable(can_write)
        }
    }
}

/// Attaches an `HX-Trigger` toast (optionally with an undo payload) to `response`.
fn toast_with_undo(
    response: &mut Response,
    level: &str,
    message: &str,
    undo: Option<serde_json::Value>,
) {
    let mut toast = serde_json::json!({ "level": level, "message": message });
    if let Some(undo) = undo {
        toast["undo"] = undo;
    }
    let payload = serde_json::json!({ "toast": toast });
    if let Ok(value) = header::HeaderValue::from_str(&ascii_json(&payload)) {
        response
            .headers_mut()
            .insert(header::HeaderName::from_static("hx-trigger"), value);
    }
}

// ---------------------------------------------------------------------------
// US-3: page + state fragment
// ---------------------------------------------------------------------------

/// `GET /control` — the control panel page (`control.read`).
pub async fn control_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.read")?;
    let view = load_view(&state, &ctx).await;
    render(&ControlPage {
        csrf_token: ctx.csrf_token.clone(),
        view,
    })
}

/// `GET /control/state` — the `control-state` fragment (`control.read`).
pub async fn control_state(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.read")?;
    let view = load_view(&state, &ctx).await;
    render(&ControlStateFragment {
        csrf_token: ctx.csrf_token.clone(),
        view,
    })
}

// ---------------------------------------------------------------------------
// US-4: SSE stream + live badge
// ---------------------------------------------------------------------------

/// `GET /control/events` — SSE stream, one `state` event every 3 s (`control.read`).
///
/// The event carries no payload the client must parse: htmx listens for
/// `sse:state` and re-fetches `/control/state`; the `every 3s` trigger on the
/// container is the fallback when `EventSource` is unavailable (D15).
pub async fn control_events(
    Extension(ctx): Extension<AuthContext>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, PortalError> {
    rbac::check(&ctx, "control.read")?;
    let interval = tokio::time::interval(Duration::from_secs(3));
    let stream = IntervalStream::new(interval)
        .map(|_| Ok::<Event, Infallible>(Event::default().event("state").data("{}")));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// `GET /control/badge` — per-viewer "du bist live" fragment (`control.read`).
///
/// Only the viewer's own status is shown (D5): live when the override names the
/// viewer or a running live broadcast belongs to them.
pub async fn control_badge(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.read")?;
    let is_live = match state.control.state(Some(&ctx.caster_id)).await {
        Ok(snapshot) => snapshot.live.active_caster_id.as_deref() == Some(ctx.caster_id.as_str()),
        Err(_) => false,
    };
    render(&LiveBadgeFragment {
        badge: LiveBadgeView { is_live },
    })
}

// ---------------------------------------------------------------------------
// US-5: switch / override (+ undo toast)
// ---------------------------------------------------------------------------

/// Body of `POST /control/source`.
#[derive(Debug, Deserialize)]
pub struct SourceForm {
    /// `layer` (switch) or `caster` (live override).
    #[serde(default)]
    pub target_kind: String,
    /// The layer name, or the caster id (empty = clear the override).
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /control/source` — force a layer or set/clear the live override
/// (`control.write`). Answers with an undo toast carrying the previous values
/// as `hx-post="/control/source"` fields (K5); the 10 s window is client-side.
pub async fn control_source(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<SourceForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.write")?;
    let htmx = is_htmx(&headers);
    let reason = form
        .reason
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "Umschalten via Control Panel".to_string());

    // Snapshot the previous state so the undo toast can restore it (K5).
    let previous = state.control.state(Some(&ctx.caster_id)).await.ok();
    let undo = undo_payload(previous.as_ref());

    let outcome = if form.target_kind == "caster" {
        let caster = form.target.trim();
        let caster = (!caster.is_empty()).then_some(caster);
        state.control.live(caster, &reason).await
    } else {
        match form.target.trim() {
            "" => {
                return Err(PortalError::Validation {
                    field: "target".into(),
                    message: "target is required".into(),
                });
            }
            layer => state.control.switch(layer, &reason).await,
        }
    };

    let (level, message, include_undo): (&str, String, bool) = match outcome {
        Ok(()) => ("success", "Quelle umgeschaltet.".to_string(), true),
        Err(ControlError::LiveActive(msg)) => ("info", msg, false),
        Err(ControlError::Conflict(msg)) => ("error", msg, false),
        Err(ControlError::Upstream(_)) => {
            ("error", "Scheduler nicht erreichbar.".to_string(), false)
        }
    };

    if !htmx {
        return Ok(redirect("/control"));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    toast_with_undo(&mut response, level, &message, include_undo.then_some(undo));
    Ok(response)
}

/// Builds the undo payload from a previous state (K5): the prior override
/// (or `target_kind: caster` + empty target) wins; otherwise the prior layer.
fn undo_payload(previous: Option<&ControlState>) -> serde_json::Value {
    match previous {
        Some(state) => {
            if let Some(caster) = &state.live.override_caster_id {
                serde_json::json!({ "target_kind": "caster", "target": caster })
            } else if let Some(layer) = &state.active_layer {
                serde_json::json!({ "target_kind": "layer", "target": layer })
            } else {
                serde_json::json!({ "target_kind": "caster", "target": "" })
            }
        }
        None => serde_json::json!({ "target_kind": "caster", "target": "" }),
    }
}

// ---------------------------------------------------------------------------
// US-6: pause (live guard) + skip
// ---------------------------------------------------------------------------

/// Body of `POST /control/pause`.
#[derive(Debug, Deserialize)]
pub struct PauseForm {
    #[serde(default)]
    pub paused: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /control/pause` — pause/resume (`control.write`); live ⇒ info toast,
/// never an error page.
pub async fn control_pause(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<PauseForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.write")?;
    let paused = form.paused == "true";
    let reason = form
        .reason
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "Pause via Control Panel".to_string());

    let response = match state.control.pause(paused, &reason).await {
        Ok(()) => toast_only("success", "Pause-Status aktualisiert."),
        Err(ControlError::LiveActive(_)) => toast_only("info", "Live aktiv — Pause nicht möglich."),
        Err(ControlError::Conflict(message)) => toast_only("error", &message),
        Err(ControlError::Upstream(_)) => toast_only("error", "Scheduler nicht erreichbar."),
    };
    if is_htmx(&headers) {
        Ok(response)
    } else {
        Ok(redirect("/control"))
    }
}

/// Body of `POST /control/skip`.
#[derive(Debug, Deserialize)]
pub struct SkipForm {
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /control/skip` — skip the playing item (`control.write`).
pub async fn control_skip(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<SkipForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.write")?;
    let reason = form
        .reason
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "Skip via Control Panel".to_string());

    let response = match state.control.skip(&reason).await {
        Ok(()) => toast_only("success", "Skip gesendet."),
        Err(ControlError::LiveActive(_)) => toast_only("info", "Skip derzeit nicht möglich."),
        Err(ControlError::Conflict(message)) => toast_only("info", &message),
        Err(ControlError::Upstream(_)) => toast_only("error", "Scheduler nicht erreichbar."),
    };
    if is_htmx(&headers) {
        Ok(response)
    } else {
        Ok(redirect("/control"))
    }
}
