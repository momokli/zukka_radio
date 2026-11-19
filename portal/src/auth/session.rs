//! Session cookie: `__Host-zukka_session` (portal.md D3).
//!
//! `__Host-` requires `Secure`, `Path=/` and no `Domain`; a misconfiguration
//! then breaks the session loudly instead of silently being insecure.

use axum::http::HeaderValue;

/// Session cookie name.
pub const COOKIE_NAME: &str = "__Host-zukka_session";

/// Builds the `Set-Cookie` header value for a fresh session.
pub fn set_cookie(token: &str, max_age_secs: i64) -> HeaderValue {
    let value = format!(
        "{COOKIE_NAME}={token}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age_secs}"
    );
    HeaderValue::from_str(&value).expect("valid cookie value")
}

/// Builds the `Set-Cookie` header value that clears the session cookie.
pub fn clear_cookie() -> HeaderValue {
    let value = format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0");
    HeaderValue::from_str(&value).expect("valid cookie value")
}

/// Extracts `name` from a `Cookie` header, if present.
pub fn parse_cookie(header: Option<&str>, name: &str) -> Option<String> {
    let header = header?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_has_host_prefix_attributes() {
        let value = set_cookie("tok", 604800).to_str().unwrap().to_string();
        assert!(value.starts_with("__Host-zukka_session=tok;"));
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("Secure"));
        assert!(value.contains("SameSite=Lax"));
        assert!(value.contains("Path=/"));
        assert!(value.contains("Max-Age=604800"));
        assert!(!value.contains("Domain"), "no Domain attribute: {value}");
    }

    #[test]
    fn clear_cookie_expires() {
        let value = clear_cookie().to_str().unwrap().to_string();
        assert!(value.contains("Max-Age=0"));
        assert!(value.starts_with("__Host-zukka_session=;"));
    }

    #[test]
    fn parse_cookie_finds_value_among_others() {
        let header = "other=1; __Host-zukka_session=abc.def-ghi; tail=2";
        assert_eq!(
            parse_cookie(Some(header), COOKIE_NAME).as_deref(),
            Some("abc.def-ghi")
        );
        assert_eq!(parse_cookie(Some("nope=1"), COOKIE_NAME), None);
        assert_eq!(parse_cookie(None, COOKIE_NAME), None);
    }
}
