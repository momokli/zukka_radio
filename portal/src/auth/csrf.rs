//! CSRF protection for non-GET requests (portal.md D4).
//!
//! Authenticated routes use the per-session token `HMAC(csrf_secret, "csrf")`.
//! The login form (pre-session) uses a double-submit cookie. In both cases the
//! comparison is constant-time and a same-origin signal (`Sec-Fetch-Site` or
//! `Origin`) is required. A failure is `403` — never a redirect.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use axum::http::HeaderMap;

/// Cookie holding the login-page double-submit token.
pub const LOGIN_CSRF_COOKIE: &str = "__Host-zukka_csrf";

/// Derives the session CSRF token: `HMAC-SHA256(secret, "csrf")`, base64url.
/// Deterministic for a fixed secret (mirrors the scheduler).
pub fn session_token(csrf_secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(csrf_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(b"csrf");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// Constant-time comparison of two CSRF tokens.
pub fn tokens_match(expected: &str, presented: &str) -> bool {
    expected.as_bytes().ct_eq(presented.as_bytes()).into()
}

/// A fresh random token for the login double-submit cookie.
pub fn random_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// Checks the same-origin signal. `Sec-Fetch-Site` (when present) must be
/// `same-origin`; otherwise a present `Origin` must match `expected_origin`.
pub fn origin_ok(headers: &HeaderMap, expected_origin: &str) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return site == "same-origin";
    }
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        Some(origin) => origin == expected_origin,
        None => true, // older clients: the token check still applies
    }
}

/// Reads the `X-CSRF-Token` header (htmx sends it globally).
pub fn header_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Parses the `csrf_token` field from a urlencoded body.
pub fn form_token(body: &[u8]) -> Option<String> {
    form_urlencoded::parse(body)
        .find(|(key, _)| key == "csrf_token")
        .map(|(_, value)| value.into_owned())
}

/// Extracts the presented token from the header or the form body.
pub fn presented_token(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    header_token(headers).or_else(|| form_token(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn session_token_is_deterministic() {
        assert_eq!(session_token("s"), session_token("s"));
        assert_ne!(session_token("s"), session_token("t"));
    }

    #[test]
    fn tokens_match_constant_time() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "ab"));
    }

    #[test]
    fn origin_signal() {
        let same = headers(&[("sec-fetch-site", "same-origin")]);
        assert!(origin_ok(&same, "https://p.example"));
        let cross = headers(&[("sec-fetch-site", "cross-site")]);
        assert!(!origin_ok(&cross, "https://p.example"));
        let good_origin = headers(&[("origin", "https://p.example")]);
        assert!(origin_ok(&good_origin, "https://p.example"));
        let bad_origin = headers(&[("origin", "https://evil.example")]);
        assert!(!origin_ok(&bad_origin, "https://p.example"));
        assert!(origin_ok(&HeaderMap::new(), "https://p.example"));
    }

    #[test]
    fn presented_token_prefers_header() {
        let h = headers(&[("x-csrf-token", "hdr")]);
        assert_eq!(
            presented_token(&h, b"csrf_token=body").as_deref(),
            Some("hdr")
        );
        assert_eq!(
            presented_token(&HeaderMap::new(), b"a=1&csrf_token=b%20c").as_deref(),
            Some("b c")
        );
        assert_eq!(presented_token(&HeaderMap::new(), b"a=1"), None);
    }
}
