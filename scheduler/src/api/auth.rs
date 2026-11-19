//! Liquidsoap token auth (§2.1, §4.1, Issue #13).
//!
//! The token layer guards **only** the `/playback/*` sub-router (wired in
//! [`crate::api::playback`]); every other route stays open until the RBAC
//! issue lands. The check is fail-closed: a missing or empty configured token
//! rejects every request.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;

/// Header carrying the presented token (§4.1).
pub const TOKEN_HEADER: &str = "x-liquidsoap-token";

/// Environment variable holding the configured token.
pub const TOKEN_ENV: &str = "LIQUIDSOAP_TOKEN";

/// Fail-closed token comparison (§2.1).
///
/// Returns `true` only when a non-empty `configured` token is present **and**
/// `presented` equals it exactly (constant-time). Every other combination
/// (env unset, env empty, header missing, mismatch) yields `false`.
pub fn check_token(configured: Option<&str>, presented: Option<&str>) -> bool {
    match configured {
        Some(cfg) if !cfg.is_empty() => match presented {
            Some(p) => ct_eq(cfg.as_bytes(), p.as_bytes()),
            None => false,
        },
        _ => false,
    }
}

/// Constant-time byte comparison (manual XOR fold, no extra dependency).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Middleware rejecting unauthenticated `/playback/*` requests with `401`.
///
/// The configured token is read **at request time** so a router built once in
/// tests picks up the test token without changing the router signature.
pub async fn require_token(req: Request, next: Next) -> Result<Response, ApiError> {
    let configured = std::env::var(TOKEN_ENV).ok();
    let presented = req
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if !check_token(configured.as_deref(), presented.as_deref()) {
        return Err(ApiError::Unauthorized);
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::check_token;

    #[test]
    fn fail_closed_matrix() {
        // env unset → always false
        assert!(!check_token(None, None));
        assert!(!check_token(None, Some("x")));
        // env empty → always false
        assert!(!check_token(Some(""), None));
        assert!(!check_token(Some(""), Some("")));
        assert!(!check_token(Some(""), Some("token")));
        // configured, header missing → false
        assert!(!check_token(Some("secret"), None));
        // configured, mismatch → false
        assert!(!check_token(Some("secret"), Some("other")));
        assert!(!check_token(Some("secret"), Some("secre")));
        // configured, exact match → true
        assert!(check_token(Some("secret"), Some("secret")));
    }
}
