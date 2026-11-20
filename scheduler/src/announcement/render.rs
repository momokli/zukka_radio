//! Pure template render engine (Issue #34).
//!
//! Replaces `{key}` placeholders from a payload. There is **no** whitelist in
//! the engine: the payload *is* the catalogue — a key is known iff it exists
//! in the payload. A known key with an empty value renders as an empty string;
//! a missing key stays **byte-identical** in the output and is reported in
//! [`Rendered::unknown`]. Rendering is a single pass: a value is never
//! re-scanned, so values may safely contain `{...}`.
//!
//! Key grammar: `^[a-z][a-z0-9_]*$` — every other brace sequence (`{}`,
//! `{ show_name }`, `{a-b}`, an unterminated `{`) is plain text.

use std::collections::{BTreeMap, BTreeSet};

/// Placeholder names documented for the portal editor (`portal.md` §Template
/// editor, `docs/architecture.md` §7). Documentation only — the engine never
/// filters against this list.
pub const DOCUMENTED_PLACEHOLDERS: &[&str] = &[
    "show_name",
    "caster_name",
    "start_time",
    "end_time",
    "show_slug",
    "broadcast_title",
    "stream_url",
    "tape_title",
];

/// Result of a render run, including diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    /// The rendered text.
    pub text: String,
    /// Valid placeholders that were missing from the payload — each once, in
    /// first-occurrence order.
    pub unknown: Vec<String>,
}

/// Replaces `{key}` placeholders from `payload`. Unknown placeholders stay
/// verbatim.
pub fn render(body: &str, payload: &BTreeMap<String, String>) -> String {
    render_detailed(body, payload).text
}

/// Like [`render`], but also returns the missing placeholders.
pub fn render_detailed(body: &str, payload: &BTreeMap<String, String>) -> Rendered {
    let mut text = String::with_capacity(body.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut unknown: Vec<String> = Vec::new();

    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            // Copy one whole char (byte-safe for non-ASCII boundaries).
            let ch = body[i..].chars().next().unwrap_or('\u{fffd}');
            text.push(ch);
            i += ch.len_utf8();
            continue;
        }

        match body[i + 1..].find('}') {
            None => {
                // No closing brace: the rest is verbatim, no infinite loop.
                text.push_str(&body[i..]);
                break;
            }
            Some(rel) => {
                let j = i + 1 + rel;
                let key = &body[i + 1..j];
                if is_valid_key(key) {
                    if let Some(value) = payload.get(key) {
                        // Value is inserted as-is and never re-scanned.
                        text.push_str(value);
                        i = j + 1;
                        continue;
                    }
                    if seen.insert(key.to_string()) {
                        unknown.push(key.to_string());
                    }
                }
                // Invalid key or missing placeholder: copy the `{` and let the
                // remainder run through the normal path so a later real
                // placeholder is not swallowed.
                text.push('{');
                i += 1;
            }
        }
    }

    Rendered { text, unknown }
}

/// Flattens a JSON object into `BTreeMap<String, String>`.
///
/// String → unchanged; Number/Bool → `to_string`; Null/Object/Array →
/// skipped. Lets the sender (#37) feed `announcements.payload` directly.
pub fn payload_from_json(value: &serde_json::Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            match value {
                serde_json::Value::String(s) => {
                    map.insert(key.clone(), s.clone());
                }
                serde_json::Value::Number(n) => {
                    map.insert(key.clone(), n.to_string());
                }
                serde_json::Value::Bool(b) => {
                    map.insert(key.clone(), b.to_string());
                }
                serde_json::Value::Null
                | serde_json::Value::Object(_)
                | serde_json::Value::Array(_) => {}
            }
        }
    }
    map
}

/// `^[a-z][a-z0-9_]*$` over ASCII.
fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn replaces_known_placeholder() {
        let out = render("Hallo {caster_name}!", &payload(&[("caster_name", "Momo")]));
        assert_eq!(out, "Hallo Momo!");
    }

    #[test]
    fn multiple_and_repeated() {
        let out = render(
            "{show_name} mit {caster_name}, {show_name} live",
            &payload(&[("show_name", "Morning"), ("caster_name", "Momo")]),
        );
        assert_eq!(out, "Morning mit Momo, Morning live");
    }

    #[test]
    fn unknown_stays_verbatim() {
        let r = render_detailed("{show_name} {nope}", &payload(&[("show_name", "Momo")]));
        assert_eq!(r.text, "Momo {nope}");
        assert_eq!(r.unknown, vec!["nope"]);
    }

    #[test]
    fn empty_value_replaces_with_empty_string() {
        let out = render("a{show_name}b", &payload(&[("show_name", "")]));
        assert_eq!(out, "ab");
    }

    #[test]
    fn missing_vs_empty_are_distinct() {
        let r = render_detailed("{a} {b}", &payload(&[("a", "")]));
        assert_eq!(r.text, " {b}");
        assert_eq!(r.unknown, vec!["b"]);
    }

    #[test]
    fn no_recursion() {
        let out = render("{k}", &payload(&[("k", "{caster_name}")]));
        assert_eq!(out, "{caster_name}");
    }

    #[test]
    fn malformed_open_without_close() {
        let r = render_detailed("x {show_name", &payload(&[]));
        assert_eq!(r.text, "x {show_name");
        assert!(r.unknown.is_empty());
    }

    #[test]
    fn empty_braces_and_spaces_verbatim() {
        let p = payload(&[("show_name", "Momo")]);
        assert_eq!(render("{}", &p), "{}");
        assert_eq!(render("{ show_name }", &p), "{ show_name }");
        assert_eq!(render("{a-b}", &p), "{a-b}");
        let r = render_detailed("{} { show_name } {a-b}", &p);
        assert!(r.unknown.is_empty());
    }

    #[test]
    fn double_braces() {
        let out = render("{{show_name}}", &payload(&[("show_name", "Momo")]));
        assert_eq!(out, "{Momo}");
    }

    #[test]
    fn unicode_safe() {
        let out = render("🎙️ {show_name} 🎶", &payload(&[("show_name", "Momo")]));
        assert_eq!(out, "🎙️ Momo 🎶");
        // A placeholder-free unicode body is copied byte-for-byte.
        assert_eq!(render("nur 🎧 Emojis", &payload(&[])), "nur 🎧 Emojis");
    }

    #[test]
    fn detailed_unknown_deduped_in_order() {
        let r = render_detailed("{b} {a} {b}", &payload(&[]));
        assert_eq!(r.unknown, vec!["b", "a"]);
    }

    #[test]
    fn payload_from_json_skips_null_and_nested() {
        let map = payload_from_json(&json!({
            "a": "x",
            "n": 3,
            "b": true,
            "z": null,
            "o": {"inner": 1},
            "arr": [1, 2]
        }));
        assert_eq!(map.get("a").map(String::as_str), Some("x"));
        assert_eq!(map.get("n").map(String::as_str), Some("3"));
        assert_eq!(map.get("b").map(String::as_str), Some("true"));
        assert!(!map.contains_key("z"));
        assert!(!map.contains_key("o"));
        assert!(!map.contains_key("arr"));
    }
}
