//! Static assets embedded into the binary and served from `/assets/{*path}`
//! (Issue #26, M3; plan E1).
//!
//! CSS, JS and fonts are `include_str!`/`include_bytes!` constants, so the
//! single binary is the only deploy artefact (no `file_server`, no volume). The
//! shell references them with `?v=<CARGO_PKG_VERSION>`, which makes the
//! immutable cache header safe (a deploy changes the URL).

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// Retro stylesheet.
pub const APP_CSS: &str = include_str!("../assets/app.css");
/// Shell bootstrap + feed rendering.
pub const APP_MJS: &str = include_str!("../assets/app.mjs");
/// ZKWV decode / downsample / canvas drawing.
pub const PEAKS_MJS: &str = include_str!("../assets/peaks.mjs");
/// Bottom-player transport.
pub const PLAYER_MJS: &str = include_str!("../assets/player.mjs");

/// `VT323` pixel font (OFL).
pub const FONT_VT323: &[u8] = include_bytes!("../assets/fonts/VT323.woff2");
/// `Space Mono` regular (OFL).
pub const FONT_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/SpaceMono-Regular.woff2");
/// `Space Mono` bold (OFL).
pub const FONT_MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/SpaceMono-Bold.woff2");
/// Font licence text (OFL).
pub const FONT_LICENSE: &[u8] = include_bytes!("../assets/fonts/OFL.txt");

/// Asset payloads are immutable and content-addressed by `?v=`.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Resolves an asset path to `(content_type, bytes)`; `None` → `404`.
fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "app.css" => Some(("text/css; charset=utf-8", APP_CSS.as_bytes())),
        "app.mjs" => Some(("text/javascript; charset=utf-8", APP_MJS.as_bytes())),
        "peaks.mjs" => Some(("text/javascript; charset=utf-8", PEAKS_MJS.as_bytes())),
        "player.mjs" => Some(("text/javascript; charset=utf-8", PLAYER_MJS.as_bytes())),
        "fonts/VT323.woff2" => Some(("font/woff2", FONT_VT323)),
        "fonts/SpaceMono-Regular.woff2" => Some(("font/woff2", FONT_MONO_REGULAR)),
        "fonts/SpaceMono-Bold.woff2" => Some(("font/woff2", FONT_MONO_BOLD)),
        "fonts/OFL.txt" => Some(("text/plain; charset=utf-8", FONT_LICENSE)),
        _ => None,
    }
}

/// `GET /assets/{*path}` — serve an embedded asset with cache + MIME headers.
pub async fn get_asset(Path(path): Path<String>) -> Response {
    match lookup(path.as_str()) {
        Some((content_type, bytes)) => {
            let mut response = Response::new(Body::from(bytes));
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            response
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_assets_resolve_with_expected_types() {
        assert_eq!(lookup("app.css").unwrap().0, "text/css; charset=utf-8");
        assert_eq!(
            lookup("app.mjs").unwrap().0,
            "text/javascript; charset=utf-8"
        );
        assert_eq!(lookup("fonts/VT323.woff2").unwrap().0, "font/woff2");
        assert!(lookup("fonts/OFL.txt").is_some());
        assert!(lookup("nope.css").is_none());
    }

    #[test]
    fn embedded_content_is_non_empty() {
        assert!(!APP_CSS.is_empty());
        assert!(!APP_MJS.is_empty());
        assert!(!PEAKS_MJS.is_empty());
        assert!(!PLAYER_MJS.is_empty());
        assert!(FONT_VT323.len() > 1024);
    }
}
