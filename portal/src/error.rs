//! Portal error type → HTML error page (§4.6 analogue, P0).
//!
//! The portal is HTML-only, so errors render a page instead of a JSON
//! envelope. `401`/`403` are produced by the auth/CSRF layers, `429` by the
//! login rate limiter, `502` when the scheduler is unreachable.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};

use crate::templates::ErrorPage;

/// Portal error.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// Not authenticated → `401`.
    #[error("unauthorized")]
    Unauthorized,
    /// Authenticated but not allowed (CSRF or RBAC) → `403`.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// Malformed request → `400`.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Not found → `404`.
    #[error("not found")]
    NotFound,
    /// Upload exceeds `UPLOAD_MAX_BYTES` → `413` (portal.md D6).
    #[error("payload too large")]
    PayloadTooLarge,
    /// Validation failed → `422` (field-level).
    #[error("validation failed for {field}: {message}")]
    Validation { field: String, message: String },
    /// Hard conflict (e.g. an illegal broadcast transition) → `409`.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Rate limit exceeded → `429`.
    #[error("too many requests")]
    TooManyRequests,
    /// The scheduler is unreachable or answered unexpectedly → `502`.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// Unexpected internal failure → `500`.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl PortalError {
    /// HTTP status for this error.
    pub fn status(&self) -> StatusCode {
        match self {
            PortalError::Unauthorized => StatusCode::UNAUTHORIZED,
            PortalError::Forbidden(_) => StatusCode::FORBIDDEN,
            PortalError::BadRequest(_) => StatusCode::BAD_REQUEST,
            PortalError::NotFound => StatusCode::NOT_FOUND,
            PortalError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            PortalError::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            PortalError::Conflict(_) => StatusCode::CONFLICT,
            PortalError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            PortalError::Upstream(_) => StatusCode::BAD_GATEWAY,
            PortalError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// A short, user-facing message (no internal detail leaks for 5xx).
    pub fn user_message(&self) -> String {
        match self {
            PortalError::Unauthorized => "Bitte zuerst anmelden.".to_string(),
            PortalError::Forbidden(_) => "Zugriff verweigert.".to_string(),
            PortalError::BadRequest(_) => "Ungültige Anfrage.".to_string(),
            PortalError::NotFound => "Nicht gefunden.".to_string(),
            PortalError::PayloadTooLarge => "Datei zu groß.".to_string(),
            PortalError::Validation { message, .. } => message.clone(),
            PortalError::Conflict(message) => message.clone(),
            PortalError::TooManyRequests => {
                "Zu viele Fehlversuche. Bitte später erneut versuchen.".to_string()
            }
            PortalError::Upstream(_) => "Dienst vorübergehend nicht erreichbar.".to_string(),
            PortalError::Internal(err) => {
                tracing::error!("internal portal error: {err:#}");
                "Interner Fehler.".to_string()
            }
        }
    }
}

impl From<sqlx::Error> for PortalError {
    fn from(err: sqlx::Error) -> Self {
        PortalError::Internal(anyhow::Error::new(err))
    }
}

impl From<askama::Error> for PortalError {
    fn from(err: askama::Error) -> Self {
        PortalError::Internal(anyhow::Error::new(err))
    }
}

impl IntoResponse for PortalError {
    fn into_response(self) -> Response {
        // Validation errors are rendered inline by their handler; if one slips
        // through, render the generic error page with the field message.
        let status = self.status();
        let page = ErrorPage {
            status: status.as_u16(),
            message: self.user_message(),
        };
        match page.render() {
            Ok(html) => (status, Html(html)).into_response(),
            Err(err) => {
                tracing::error!("failed to render error page: {err}");
                (status, Html("error".to_string())).into_response()
            }
        }
    }
}

/// A `303 See Other` redirect to `location`.
pub fn redirect(location: &str) -> Response {
    Redirect::to(location).into_response()
}
