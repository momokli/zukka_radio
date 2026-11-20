//! ETag computation and `If-None-Match` matching (`docs/tapes.md` §3.5).
//!
//! An ETag is `"<hex sha256 prefix>"`, derived from `updated_at` + `slug` for a
//! single tape and from the visible page content for the feed. A matching
//! `If-None-Match` yields `304` with the same `ETag`/`Cache-Control` headers.

use axum::http::HeaderMap;
use axum::http::header::IF_NONE_MATCH;
use sha2::{Digest, Sha256};

fn hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{}\"", &hex[..32])
}

/// ETag for a single tape: `updated_at` + `slug`.
pub fn for_tape(updated_at: &str, slug: &str) -> String {
    hash(&[updated_at, slug])
}

/// ETag for a feed page: the concatenated `updated_at`/`slug`, the cursor and
/// the sort direction.
pub fn for_feed(sort: &str, next_cursor: Option<&str>, parts: &[String]) -> String {
    let mut all: Vec<&str> = vec![sort, next_cursor.unwrap_or("")];
    all.extend(parts.iter().map(String::as_str));
    hash(&all)
}

/// ETag for a waveform frame: `"w1-<slug>-<bars>"` (`docs/tapes.md` §3.6).
pub fn for_waveform(slug: &str, bars: i64) -> String {
    format!("\"w1-{slug}-{bars}\"")
}

/// True when the request's `If-None-Match` matches `etag` (or is `*`).
pub fn matches(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get(IF_NONE_MATCH) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
    })
}
