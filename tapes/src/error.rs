//! API error type and the §4.6 error envelope.
//!
//! Every failure renders the fixed envelope
//! `{ "error": { "code": "…", "message": "…" } }`. Internal causes are logged
//! but never serialized.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// Result alias used across the API and store layers.
pub type ApiResult<T> = Result<T, ApiError>;

/// Structured public-API error (consolidated-architecture §4.6).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// A referenced resource does not exist / is not public → `404 not_found`.
    #[error("{resource} {id} not found")]
    NotFound { resource: &'static str, id: String },

    /// A tape that is unknown *or* not public → the single public `404`.
    ///
    /// The message deliberately omits the slug so that an existing-but-hidden
    /// tape and a never-existing slug render a byte-identical body (no
    /// existence leak, R9).
    #[error("tape not found")]
    TapeNotFound,

    /// Unknown route → `404 not_found` (router fallback).
    #[error("route not found")]
    RouteNotFound,

    /// A malformed request (`cursor`, `from > to`, `limit`, …) → `400`.
    #[error("{message}")]
    BadRequest { message: String },

    /// The requested byte range lies outside the file → `416`.
    ///
    /// Carries the file size so the response can emit
    /// `Content-Range: bytes */{size}` (§3.5).
    #[error("range not satisfiable")]
    RangeNotSatisfiable { size: u64 },

    /// The Music API object store is unreachable/failed → `502`.
    #[error("storage unavailable")]
    UpstreamUnavailable,

    /// The database is unreachable → `503` (healthz only).
    #[error("database unavailable")]
    Unavailable,

    /// Unexpected server failure → `500` (details never leak).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    /// The single public `404` for a tape that is unknown *or* not public.
    ///
    /// Both cases render exactly the same body, so the response never leaks
    /// whether a slug exists (R9).
    pub fn tape_not_found() -> Self {
        ApiError::TapeNotFound
    }

    /// `400 bad_request` with a specific message.
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
        let mut extra: Vec<(header::HeaderName, HeaderValue)> = Vec::new();
        let (status, code, message) = match self {
            ApiError::NotFound { resource, id } => (
                StatusCode::NOT_FOUND,
                "not_found",
                format!("{resource} {id} not found"),
            ),
            ApiError::TapeNotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "tape not found".to_string(),
            ),
            ApiError::RouteNotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "route not found".to_string(),
            ),
            ApiError::BadRequest { message } => (StatusCode::BAD_REQUEST, "bad_request", message),
            ApiError::RangeNotSatisfiable { size } => {
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{size}")) {
                    extra.push((header::CONTENT_RANGE, value));
                }
                (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "range_not_satisfiable",
                    "range not satisfiable".to_string(),
                )
            }
            ApiError::UpstreamUnavailable => {
                extra.push((header::RETRY_AFTER, HeaderValue::from_static("5")));
                (
                    StatusCode::BAD_GATEWAY,
                    "storage",
                    "storage unavailable".to_string(),
                )
            }
            ApiError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "internal_error",
                "service unavailable".to_string(),
            ),
            ApiError::Internal(err) => {
                tracing::error!("internal error: {err:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_string(),
                )
            }
        };

        let mut res = (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response();
        for (name, value) in extra {
            res.headers_mut().insert(name, value);
        }
        res
    }
}

/// Convenience: the envelope body for a `503` healthz failure.
pub fn unavailable_body() -> Value {
    json!({ "error": { "code": "internal_error", "message": "database unavailable" } })
}
