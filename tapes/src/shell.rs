//! Server-rendered HTML shell for the public tape page (Issue #26, M3).
//!
//! The shell is **one** template (`assets/index.html`) with four unique markers
//! (`<!--META-->`, `<!--NOSCRIPT-->`, `<!--TAPE_STATE-->`, `<!--APP_VERSION-->`).
//! Every dynamic value is escaped *before* it reaches the template: text and
//! attributes go through [`escape_html`], the JSON island through
//! [`json_for_html`]. There is no template engine and no string built from raw
//! user data — this is the single XSS chokepoint (plan §2 R1/R2/R3).
//!
//! The JSON island (`<script type="application/json" id="tape-state">`) carries
//! the tape, so the client never needs executable inline script and the CSP can
//! stay `script-src 'self'` (plan E3/O6).

use serde::Serialize;

use crate::model::TapeDto;

/// The shell template, embedded at compile time (plan E1).
pub const INDEX_TEMPLATE: &str = include_str!("../assets/index.html");

/// Escapes `& < > " '` for both text and attribute contexts.
///
/// This is the only way untrusted text may reach the template. The five
/// replacements are the HTML5 named entities; `'` uses the numeric form so the
/// output stays byte-stable across renderers.
pub fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Serializes a value to JSON and neuters HTML/JS breakout sequences.
///
/// `serde_json` already escapes `"`, but leaves `<`, `>` and `&` literal — a
/// `</script>` inside a string would close the JSON island early. We rewrite
/// those (and U+2028/U+2029) to their `\uXXXX` form, which keeps the JSON valid
/// (they only ever occur inside string literals) and the island intact.
pub fn json_for_html<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Formats a decoded duration as `H:MM:SS`; `None`/non-positive → empty string.
///
/// Examples: `7_194_000 → "1:59:54"`, `3_600_000 → "1:00:00"`, `None → ""`.
pub fn duration_label(duration_ms: Option<i64>) -> String {
    match duration_ms {
        Some(ms) if ms > 0 => {
            let total = ms / 1000;
            let hours = total / 3600;
            let minutes = (total % 3600) / 60;
            let seconds = total % 60;
            format!("{hours}:{minutes:02}:{seconds:02}")
        }
        _ => String::new(),
    }
}

/// One `<noscript>` feed entry.
#[derive(Debug, Clone)]
pub struct NoScriptItem {
    pub slug: String,
    pub label: String,
}

/// The three injectable regions of the shell template, already escaped.
#[derive(Debug, Clone)]
pub struct PageData {
    /// `<title>` + OG/Twitter/canonical tags for `<!--META-->`.
    pub meta_html: String,
    /// `<li>` entries for `<!--NOSCRIPT-->`.
    pub noscript_html: String,
    /// Escaped JSON (or `null`) for `<!--TAPE_STATE-->`.
    pub tape_state: String,
}

/// Fills the four markers. Marker values are escaped by the callers; this
/// function only substitutes.
pub fn render(page: &PageData) -> String {
    INDEX_TEMPLATE
        .replace("<!--META-->", &page.meta_html)
        .replace("<!--NOSCRIPT-->", &page.noscript_html)
        .replace("<!--TAPE_STATE-->", &page.tape_state)
        .replace("<!--APP_VERSION-->", env!("CARGO_PKG_VERSION"))
}

/// Meta block for the feed index page (`GET /`).
pub fn index_meta(base_url: &str) -> String {
    let base = escape_html(base_url.trim_end_matches('/'));
    format!(
        "<title>radio zukka tapes</title>\n\
         <meta property=\"og:type\" content=\"website\" />\n\
         <meta property=\"og:title\" content=\"radio zukka tapes\" />\n\
         <meta property=\"og:url\" content=\"{base}/\" />\n\
         <meta name=\"twitter:card\" content=\"summary\" />\n"
    )
}

/// Meta block for a tape page (`GET /t/{slug}`), per `docs/tapes.md` §3.9.
///
/// - `og:title` / `<title>` base = `caster.name – window_label`
/// - `og:description` = `[show.name · ]duration_label`
/// - `og:url` = `share_url`, `og:audio` = `audio_url`, `og:audio:type` = `mime`
pub fn tape_meta(dto: &TapeDto) -> String {
    let title = format!("{} \u{2013} {}", dto.caster.name, dto.window_label);
    let page_title = format!("{title} // radio zukka tapes");

    let mut description_parts: Vec<String> = Vec::new();
    if let Some(show) = &dto.show {
        let name = show.name.trim();
        if !name.is_empty() {
            description_parts.push(name.to_string());
        }
    }
    let duration = duration_label(dto.duration_ms);
    if !duration.is_empty() {
        description_parts.push(duration);
    }
    let description = description_parts.join(" \u{00b7} ");

    let mut html = String::new();
    html.push_str(&format!("<title>{}</title>\n", escape_html(&page_title)));
    html.push_str("<meta property=\"og:type\" content=\"article\" />\n");
    html.push_str(&format!(
        "<meta property=\"og:title\" content=\"{}\" />\n",
        escape_html(&title)
    ));
    if !description.is_empty() {
        html.push_str(&format!(
            "<meta property=\"og:description\" content=\"{}\" />\n",
            escape_html(&description)
        ));
    }
    html.push_str(&format!(
        "<meta property=\"og:url\" content=\"{}\" />\n",
        escape_html(&dto.share_url)
    ));
    html.push_str(&format!(
        "<meta property=\"og:audio\" content=\"{}\" />\n",
        escape_html(&dto.audio_url)
    ));
    if let Some(mime) = dto.mime.as_deref() {
        if !mime.trim().is_empty() {
            html.push_str(&format!(
                "<meta property=\"og:audio:type\" content=\"{}\" />\n",
                escape_html(mime)
            ));
        }
    }
    html.push_str("<meta name=\"twitter:card\" content=\"summary_large_image\" />\n");
    html.push_str(&format!(
        "<link rel=\"canonical\" href=\"{}\" />\n",
        escape_html(&dto.share_url)
    ));
    html
}

/// Renders the `<li>` list used by the `<noscript>` block (already escaped).
pub fn noscript_html(items: &[NoScriptItem]) -> String {
    items
        .iter()
        .map(|item| {
            format!(
                "<li><a href=\"/t/{}\">{}</a></li>",
                escape_html(&item.slug),
                escape_html(&item.label)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Standalone, minimal HTML 404 page.
///
/// Deliberately **does not** use the shell template and never receives the
/// requested slug, so an unknown path and a hidden tape render byte-identical
/// output with no reflection vector (plan E4, R3).
pub fn render_not_found() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "<!doctype html>\n\
<html lang=\"de\">\n\
<head>\n\
<meta charset=\"utf-8\" />\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
<title>404 // radio zukka tapes</title>\n\
<link rel=\"stylesheet\" href=\"/assets/app.css?v={version}\" />\n\
</head>\n\
<body class=\"not-found\">\n\
<main class=\"not-found-main\">\n\
<p class=\"not-found-code\">404</p>\n\
<h1>Seite nicht gefunden</h1>\n\
<p><a href=\"/\">zurück zum Feed</a></p>\n\
</main>\n\
</body>\n\
</html>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_covers_all_five_entities() {
        assert_eq!(
            escape_html("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert!(!escape_html("<script>").contains('<'));
        assert_eq!(escape_html("a \" b"), "a &quot; b");
        assert_eq!(escape_html("it's"), "it&#39;s");
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    #[test]
    fn json_for_html_neutralises_script_breakout() {
        let out = json_for_html(&"</script><script>alert(1)</script>");
        assert!(!out.contains("</"), "no literal close tag: {out}");
        assert!(!out.contains('<'));
        assert!(!out.contains('>'));
        // Still valid JSON and parses back to the original string.
        let back: String = serde_json::from_str(&out).expect("valid json");
        assert_eq!(back, "</script><script>alert(1)</script>");
    }

    #[test]
    fn json_for_html_escapes_ampersand_and_line_separators() {
        let out = json_for_html(&"a & b \u{2028} c \u{2029}");
        assert!(!out.contains('&'));
        assert!(!out.contains('\u{2028}'));
        assert!(!out.contains('\u{2029}'));
        let back: String = serde_json::from_str(&out).expect("valid json");
        assert_eq!(back, "a & b \u{2028} c \u{2029}");
    }

    #[test]
    fn duration_label_examples() {
        assert_eq!(duration_label(Some(7_194_000)), "1:59:54");
        assert_eq!(duration_label(Some(3_600_000)), "1:00:00");
        assert_eq!(duration_label(Some(1_800_000)), "0:30:00");
        assert_eq!(duration_label(None), "");
        assert_eq!(duration_label(Some(0)), "");
        assert_eq!(duration_label(Some(-5)), "");
    }

    #[test]
    fn not_found_page_has_no_slug_and_links_home() {
        let page = render_not_found();
        assert!(page.contains("href=\"/\""));
        assert!(page.contains("404"));
        assert!(!page.contains("<script"));
    }

    #[test]
    fn render_substitutes_every_marker() {
        let page = PageData {
            meta_html: "<title>x</title>".to_string(),
            noscript_html: "<li>x</li>".to_string(),
            tape_state: "null".to_string(),
        };
        let html = render(&page);
        assert!(!html.contains("<!--META-->"));
        assert!(!html.contains("<!--NOSCRIPT-->"));
        assert!(!html.contains("<!--TAPE_STATE-->"));
        assert!(!html.contains("<!--APP_VERSION-->"));
        assert!(html.contains("/assets/app.css?v="));
    }
}
