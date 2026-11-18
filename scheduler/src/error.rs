//! API error type and the §4.6 error envelope.
//!
//! Every handler returns [`ApiError`] on failure; [`ApiError::into_response`]
//! renders the fixed envelope:
//!
//! ```json
//! { "error": { "code": "…", "message": "…", "details": { "field": "…" } } }
//! ```

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

/// Result alias used across the API and store layers.
pub type ApiResult<T> = Result<T, ApiError>;

/// Structured API error (consolidated-architecture §4.6).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// A referenced resource does not exist → `404 not_found`.
    #[error("{resource} {id} not found")]
    NotFound { resource: &'static str, id: String },

    /// Unknown route → `404 not_found` (router fallback).
    #[error("route not found")]
    RouteNotFound,

    /// A duplicate, version conflict or illegal state transition → `409`.
    ///
    /// `code` is one of `version_conflict`, `duplicate_name`,
    /// `duplicate_position`, `queue_in_use`.
    #[error("{message}")]
    Conflict {
        code: &'static str,
        message: String,
        details: Option<Value>,
    },

    /// A field failed validation → `422 validation_error`.
    #[error("validation failed for {field}: {reason}")]
    Validation { field: &'static str, reason: String },

    /// A malformed or unparseable request → `400 bad_request`.
    #[error("{message}")]
    BadRequest { message: String },

    /// Unexpected server failure → `500 internal_error` (details never leak).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    /// `404 not_found` for `resource` / `id`.
    pub fn not_found(resource: &'static str, id: impl Into<String>) -> Self {
        ApiError::NotFound {
            resource,
            id: id.into(),
        }
    }

    /// `409` with a specific machine code and no extra details.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        ApiError::Conflict {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// `422 validation_error` for `field`.
    pub fn validation(field: &'static str, reason: impl Into<String>) -> Self {
        ApiError::Validation {
            field,
            reason: reason.into(),
        }
    }

    /// `400 bad_request`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::BadRequest {
            message: message.into(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        ApiError::Internal(anyhow::Error::new(err))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, details) = match self {
            ApiError::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "not_found",
                format!("{resource} {id} not found"),
                None,
            ),
            ApiError::RouteNotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "route not found".to_string(),
                None,
            ),
            ApiError::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details),
            ApiError::Validation { field, reason } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                reason,
                Some(json!({ "field": field })),
            ),
            ApiError::BadRequest { message } => {
                (StatusCode::BAD_REQUEST, "bad_request", message, None)
            }
            ApiError::Internal(err) => {
                tracing::error!("internal error: {err:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_string(),
                    None,
                )
            }
        };

        let mut error = Map::new();
        error.insert("code".to_string(), json!(code));
        error.insert("message".to_string(), json!(message));
        if let Some(details) = details {
            error.insert("details".to_string(), details);
        }

        (status, Json(json!({ "error": Value::Object(error) }))).into_response()
    }
}

/// Returns the SQLite message when `err` is a `UNIQUE` constraint violation.
pub fn unique_violation(err: &sqlx::Error) -> Option<String> {
    match err {
        sqlx::Error::Database(db) if db.message().contains("UNIQUE constraint failed") => {
            Some(db.message().to_string())
        }
        _ => None,
    }
}

/// True when `err` is a `FOREIGN KEY` constraint violation.
pub fn is_foreign_key_violation(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db) if db.message().contains("FOREIGN KEY constraint failed")
    )
}
