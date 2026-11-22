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

    /// Missing or invalid service token → `401 unauthorized` (§4.6, Issue #13).
    #[error("unauthorized")]
    Unauthorized,

    /// Authenticated but not allowed → `403 forbidden` (§4.6).
    #[error("forbidden")]
    Forbidden,

    /// Rate limit exceeded → `429 too_many_requests` (§4.6, portal.md D3).
    #[error("too many requests")]
    TooManyRequests,

    /// A command-proxy error carrying an explicit §6.4 contract code + status
    /// (Issue #147): `unknown_command` (400), `channel_not_found` (404),
    /// `invalid_args`/`bad_gain`/`bad_source` (422), `command_failed` (502),
    /// `engine_unavailable` (503).
    #[error("{message}")]
    Contract {
        status: StatusCode,
        code: &'static str,
        message: String,
        details: Option<Value>,
    },

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

    // --- Issue #147: command-proxy contract errors (§6.4) ------------------

    /// `400 unknown_command` — `cmd` not in the proxy whitelist.
    pub fn unknown_command(cmd: &str) -> Self {
        ApiError::Contract {
            status: StatusCode::BAD_REQUEST,
            code: "unknown_command",
            message: format!("unknown command '{cmd}'"),
            details: None,
        }
    }

    /// `422 invalid_args` — missing/malformed `args` (`details.field`).
    pub fn invalid_args(field: &'static str, message: impl Into<String>) -> Self {
        ApiError::Contract {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_args",
            message: message.into(),
            details: Some(json!({ "field": field })),
        }
    }

    /// `404 channel_not_found` — an unknown channel/layer id.
    pub fn channel_not_found(id: &str) -> Self {
        ApiError::Contract {
            status: StatusCode::NOT_FOUND,
            code: "channel_not_found",
            message: format!("channel '{id}' not found"),
            details: None,
        }
    }

    /// `422 bad_gain` — gain outside `[-60,+12]` dB / linear outside `[0,1]`.
    pub fn bad_gain(field: &'static str, message: impl Into<String>) -> Self {
        ApiError::Contract {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "bad_gain",
            message: message.into(),
            details: Some(json!({ "field": field })),
        }
    }

    /// `422 bad_source` — an invalid `source` prefix.
    pub fn bad_source(message: impl Into<String>) -> Self {
        ApiError::Contract {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "bad_source",
            message: message.into(),
            details: Some(json!({ "field": "source" })),
        }
    }

    /// `502 command_failed` — the engine reported an unmapped `ERROR …`.
    pub fn command_failed(message: impl Into<String>) -> Self {
        ApiError::Contract {
            status: StatusCode::BAD_GATEWAY,
            code: "command_failed",
            message: message.into(),
            details: None,
        }
    }

    /// `503 engine_unavailable` — no URL configured or a transport failure.
    pub fn engine_unavailable(message: impl Into<String>) -> Self {
        ApiError::Contract {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "engine_unavailable",
            message: message.into(),
            details: None,
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        // Count SQLite contention (§3.8, `zukka_scheduler_db_locked_total`).
        if crate::metrics::is_db_locked(&err) {
            crate::metrics::record_scheduler_db_locked();
        }
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
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or invalid token".to_string(),
                None,
            ),
            ApiError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "forbidden".to_string(),
                None,
            ),
            ApiError::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_requests",
                "too many requests".to_string(),
                None,
            ),
            ApiError::Contract {
                status,
                code,
                message,
                details,
            } => (status, code, message, details),
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
