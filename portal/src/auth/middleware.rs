//! Auth + CSRF middleware.
//!
//! `resolve_session` validates the cookie against the scheduler and inserts an
//! [`AuthContext`]. `require_csrf` guards non-GET requests of authenticated
//! routes (token + same-origin signal).

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
use crate::auth::AuthContext;
use crate::auth::csrf;
use crate::auth::session;
use crate::error::{PortalError, redirect};

/// Maximum body size the CSRF layer buffers for token extraction.
const CSRF_BODY_LIMIT: usize = 64 * 1024;

/// Resolves the session from the cookie. Unauthenticated requests are
/// redirected to `/login` (GET) or answered with `401` (non-GET); the stale
/// cookie is cleared.
pub async fn resolve_session(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, PortalError> {
    let cookie = req
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok());
    let token = session::parse_cookie(cookie, session::COOKIE_NAME);

    let Some(token) = token else {
        return Ok(unauthenticated(req.method()));
    };

    match state.identity.validate_session(&token).await {
        Ok(Some(validation)) => {
            let caster = validation.caster;
            let ctx = AuthContext {
                session_id: validation.session_id,
                caster_id: caster.id,
                username: caster.username,
                display_name: caster.display_name,
                role: caster.role,
                permissions: caster.permissions,
                csrf_token: validation.csrf_token,
            };
            req.extensions_mut().insert(ctx);
            Ok(next.run(req).await)
        }
        Ok(None) => Ok(unauthenticated(req.method())),
        Err(err) => {
            tracing::warn!("session validation failed: {err}");
            Err(PortalError::Upstream(err.to_string()))
        }
    }
}

/// Response for an unauthenticated request: `303 /login` for GET, `401` else.
fn unauthenticated(method: &Method) -> Response {
    if method == Method::GET {
        return redirect("/login");
    }
    let mut response = (StatusCode::UNAUTHORIZED, "Bitte zuerst anmelden.").into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, session::clear_cookie());
    response
}

/// Guards non-GET requests: verifies the CSRF token and the same-origin signal.
/// A failure is `403` — never a redirect.
pub async fn require_csrf(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, PortalError> {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(next.run(req).await);
    }

    let ctx = req
        .extensions()
        .get::<AuthContext>()
        .cloned()
        .ok_or(PortalError::Unauthorized)?;

    if !csrf::origin_ok(req.headers(), state.config.origin()) {
        return Err(PortalError::Forbidden("cross-origin request".into()));
    }

    // Fast path: htmx sends the token in a header for every request, so large
    // bodies (e.g. multipart uploads) never need to be buffered here.
    if let Some(presented) = csrf::header_token(req.headers()) {
        if csrf::tokens_match(&ctx.csrf_token, &presented) {
            return Ok(next.run(req).await);
        }
        return Err(PortalError::Forbidden("invalid CSRF token".into()));
    }

    // Fallback: classic forms carry the token in a urlencoded body.
    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, CSRF_BODY_LIMIT)
        .await
        .map_err(|err| PortalError::BadRequest(err.to_string()))?;
    let presented = csrf::presented_token(&parts.headers, &bytes);
    let ok = presented
        .map(|token| csrf::tokens_match(&ctx.csrf_token, &token))
        .unwrap_or(false);
    if !ok {
        return Err(PortalError::Forbidden("invalid CSRF token".into()));
    }

    let req = Request::from_parts(parts, Body::from(bytes));
    Ok(next.run(req).await)
}
