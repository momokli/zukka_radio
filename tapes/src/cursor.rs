//! Opaque keyset cursor: `base64url(json!({"t": <started_at_ms>, "s": "<slug>"}))`.
//!
//! The cursor carries the last row's `started_at` (Unix ms UTC) and `slug`, so
//! the next page uses a strict keyset comparison on
//! `(started_at, slug)` — no offset, no overlap (D16, §3.4).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// Keyset position of the last returned tape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// `started_at` in Unix milliseconds (UTC).
    pub t: i64,
    /// The tape slug at that position.
    pub s: String,
}

/// Encodes a cursor as unpadded base64url.
pub fn encode(cursor: &Cursor) -> String {
    let json = serde_json::to_vec(cursor).expect("cursor serialization is infallible");
    URL_SAFE_NO_PAD.encode(json)
}

/// Decodes an opaque cursor; malformed input → `400 bad_request`.
pub fn decode(raw: &str) -> Result<Cursor, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| ApiError::bad_request("invalid cursor: not valid base64url"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad_request("invalid cursor: malformed payload"))
}
