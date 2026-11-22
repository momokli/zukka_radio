//! Minimal control debug UI (Issue #149).
//!
//! `GET /control/debug` renders a standalone page that drives the cockpit
//! **exclusively** through the WebSocket bridge `/api/v1/control/ws` (no SSE,
//! no polling, no REST reads). The page is a throwaway placeholder that will be
//! replaced by the real cockpit; it exists to exercise the bridge end to end.
//!
//! The handler is a thin shell: it only guards on `control.read` and renders
//! static HTML. All live state arrives in the browser over the bridge, so the
//! server side holds no control state and talks to no backend.

use askama::Template;
use axum::Extension;
use axum::response::{Html, IntoResponse, Response};

use crate::auth::AuthContext;
use crate::error::PortalError;
use crate::rbac;
use crate::templates::ControlDebugPage;

/// `GET /control/debug` — minimal debug mixer UI (`control.read`).
///
/// A GET request is CSRF-free (the `protected` layer only guards non-GET).
pub async fn control_debug_get(
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "control.read")?;
    let page = ControlDebugPage {
        can_write: ctx.has("control.write"),
    };
    Ok(Html(page.render()?).into_response())
}
