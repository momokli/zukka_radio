//! Icecast observation layer (Issue #140).
//!
//! The scheduler runs exactly **one** `output.icecast` mount (`/stream.mp3`,
//! `liquidsoap/lib/outputs.liq`); every priority layer (live/show/queue/
//! rotation/relay/fun) is mixed into that single source. A 1:1 mount→layer
//! mapping therefore does not exist today — [`mount_layer`] is only an
//! extension point and returns `None` for the output mount.
//!
//! What Icecast *does* expose per source is authoritative for the actual
//! **output**: whether anything is on air, how many listeners, `stream_start`
//! and the ICY metadata (`title`/`artist`) of the audible stream. The layer is
//! derived from those titles plus DB/settings signals and is always marked as
//! *observed* (never as an authoritative layer report).
//!
//! Everything in this module is fail-open: an unreachable or malformed status
//! document yields `None` and never a 5xx (see [`observe_icecast`]).

use std::time::Duration;

use serde_json::Value;

use crate::dto::{ObservedNowPlaying, SourceLayer};

/// Env var holding the Icecast status URL (#42, unprefixed — set in
/// `compose/*.yml` from `ZUKKA_ICECAST_STATUS_URL`).
pub const ICECAST_STATUS_URL_ENV: &str = "ICECAST_STATUS_URL";

/// Default Icecast status endpoint: scheduler and icecast share the `back`
/// network, where the service is reachable as `icecast:8000`.
const DEFAULT_ICECAST_STATUS_URL: &str = "http://icecast:8000/status-json.xsl";

/// Icecast poll timeout — observation must stay cheap (< 1 s).
const ICECAST_TIMEOUT: Duration = Duration::from_secs(1);

/// One snapshot of the Icecast `status-json.xsl` document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IcecastSnapshot {
    /// One entry per published source (usually exactly the output mount).
    pub sources: Vec<ObservedSource>,
}

/// One source entry of the Icecast status document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedSource {
    /// Mount URL as reported by Icecast (`mount`, `listenurl` or `source`).
    pub mount: Option<String>,
    /// ICY title of the audible stream, if any.
    pub title: Option<String>,
    /// ICY artist of the audible stream, if any.
    pub artist: Option<String>,
    /// Current listener count (`0` when absent/non-numeric).
    pub listeners: u64,
    /// Icecast `stream_start`, if reported.
    pub stream_start: Option<String>,
    /// Icecast `server_name`, if reported.
    pub server_name: Option<String>,
}

/// The observed output state (always marked as derived-by-observation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Whether Icecast reports a live source (audible output).
    pub on_air: bool,
    /// Heuristically derived layer, or `None` when it cannot be named.
    pub layer: Option<SourceLayer>,
    /// Stable machine-readable reason (`icecast_unreachable`, `silent`, `live`,
    /// `db_match`, `rotation_match`, `external`, `relay_setting`, `unknown`).
    pub reason: &'static str,
    /// ICY now-playing, when the source carries title/artist metadata.
    pub now_playing: Option<ObservedNowPlaying>,
    /// Mount of the observed source (extension point for per-layer mounts).
    pub mount: Option<String>,
}

/// Signals the layer derivation may consult, gathered by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedHints {
    /// DB now-playing (queue/show/tape), or `None`.
    pub db_now_playing: Option<ObservedNowPlaying>,
    /// Live layer is active (override or running live broadcast).
    pub live_active: bool,
    /// `settings['control.relay_active'] == "true"`.
    pub relay_active_setting: bool,
    /// `settings['control.fun_active'] == "true"`.
    pub fun_active_setting: bool,
    /// Kind of the running broadcast (`recording`, `playlist`, `live`, …).
    pub running_kind: Option<String>,
    /// Titles of the shared library (`tracks.visibility = 'shared'`).
    pub shared_titles: Vec<String>,
}

/// Parses an Icecast `status-json.xsl` body.
///
/// Tolerant like the listener sum it replaces: `icestats.source` may be a
/// single object or an array, missing fields become `None`/`0`. Returns `None`
/// only when the body is not valid JSON at all.
pub fn parse_snapshot(body: &str) -> Option<IcecastSnapshot> {
    let value: Value = serde_json::from_str(body).ok()?;
    let source = match value.get("icestats").and_then(|stats| stats.get("source")) {
        Some(source) => source,
        // No `icestats.source` ⇒ no streams.
        None => return Some(IcecastSnapshot::default()),
    };
    let items: Vec<&Value> = match source {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![source],
        // `source` present but neither object nor array (`null`, string, …).
        _ => return Some(IcecastSnapshot::default()),
    };
    let sources = items
        .into_iter()
        .filter(|item| item.is_object())
        .map(|item| ObservedSource {
            mount: string_field(item, "mount")
                .or_else(|| string_field(item, "listenurl"))
                .or_else(|| string_field(item, "source")),
            title: string_field(item, "title"),
            artist: string_field(item, "artist"),
            listeners: item.get("listeners").and_then(Value::as_u64).unwrap_or(0),
            stream_start: string_field(item, "stream_start"),
            server_name: string_field(item, "server_name"),
        })
        .collect();
    Some(IcecastSnapshot { sources })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Maps an Icecast mount to a priority layer.
///
/// Today all layers share the single `output.icecast` mount `/stream.mp3`, so
/// the mount carries no layer information and this returns `None`. Kept as the
/// extension point for a future per-layer mount layout (see §4.4).
pub fn mount_layer(mount: &str) -> Option<SourceLayer> {
    let path = mount
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    match path.as_str() {
        // Single output mount: layer is not encoded in the mount.
        "/stream.mp3" | "stream.mp3" | "/" | "" => None,
        _ => None,
    }
}

/// Case/spacing/punctuation-insensitive title key used for equality checks.
pub fn normalize_title(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when the ICY metadata matches a local title.
///
/// Matches either the bare title (`db_title`) or the combined
/// `artist - title` form Icecast often reports for relayed streams.
fn title_matches(icy_title: &str, local_title: &str, local_artist: Option<&str>) -> bool {
    let icy = normalize_title(icy_title);
    if icy.is_empty() {
        return false;
    }
    let title = normalize_title(local_title);
    if !title.is_empty() && icy == title {
        return true;
    }
    if let Some(artist) = local_artist.filter(|artist| !artist.trim().is_empty()) {
        let combined = normalize_title(&format!("{artist} - {local_title}"));
        if !combined.is_empty() && icy == combined {
            return true;
        }
    }
    false
}

/// Derives the observed layer from an Icecast snapshot plus local hints.
///
/// Priority (highest first):
/// 1. snapshot unreachable → `on_air = false`, no layer, `icecast_unreachable`;
/// 2. snapshot without source → `on_air = false`, `silent`;
/// 3. live layer active → `live`;
/// 4. ICY title matches DB now-playing → `show` (recording) else `queue`;
/// 5. ICY title matches a shared track → `rotation` (with artist confirmation
///    or an exact title match, never a fuzzy guess);
/// 6. ICY title/artist present without local match → `relay` (external);
/// 7. on air with `relay_active` setting → `relay`;
/// 8. otherwise no layer, `unknown`.
pub fn derive_observed_layer(
    snapshot: Option<&IcecastSnapshot>,
    hints: &ObservedHints,
) -> Observed {
    let Some(snapshot) = snapshot else {
        return Observed {
            on_air: false,
            layer: None,
            reason: "icecast_unreachable",
            now_playing: None,
            mount: None,
        };
    };
    let Some(source) = snapshot.sources.first() else {
        return Observed {
            on_air: false,
            layer: None,
            reason: "silent",
            now_playing: None,
            mount: None,
        };
    };

    let title = source
        .title
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let artist = source
        .artist
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let has_icy = title.is_some() || artist.is_some();
    let now_playing = has_icy.then(|| ObservedNowPlaying {
        title: title.map(str::to_string),
        artist: artist.map(str::to_string),
        source: Some("icecast".to_string()),
    });

    let observed = |layer: SourceLayer, reason: &'static str| Observed {
        on_air: true,
        layer: Some(layer),
        reason,
        now_playing: now_playing.clone(),
        mount: source.mount.clone(),
    };

    // (3) live layer wins over any title heuristic.
    if hints.live_active {
        return observed(SourceLayer::Live, "live");
    }

    // (4) ICY title matches the DB now-playing item.
    if let (Some(icy), Some(db)) = (title, hints.db_now_playing.as_ref()) {
        if title_matches(
            icy,
            &db.title.clone().unwrap_or_default(),
            db.artist.as_deref(),
        ) {
            let layer = if hints.running_kind.as_deref() == Some("recording") {
                SourceLayer::Show
            } else {
                SourceLayer::Queue
            };
            return observed(layer, "db_match");
        }
    }

    // (5) ICY title matches a shared library track → rotation. An exact title
    // match is required (Q3 mitigation: never upgrade a near-match to rotation).
    if let Some(icy) = title {
        if hints
            .shared_titles
            .iter()
            .any(|local| title_matches(icy, local, None))
        {
            return observed(SourceLayer::Rotation, "rotation_match");
        }
    }

    // (6) ICY metadata without a local match ⇒ external relay (SomaFM always
    // carries metadata). Q3 mitigation: prefer relay over a false rotation.
    if has_icy {
        return observed(SourceLayer::Relay, "external");
    }

    // (7) on air, no metadata, relay opt-in setting.
    if hints.relay_active_setting {
        return observed(SourceLayer::Relay, "relay_setting");
    }

    // (8) on air but nothing identifies the layer.
    Observed {
        on_air: true,
        layer: None,
        reason: "unknown",
        now_playing,
        mount: source.mount.clone(),
    }
}

/// Configured Icecast status URL, or the in-network default.
fn status_url() -> String {
    std::env::var(ICECAST_STATUS_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ICECAST_STATUS_URL.to_string())
}

/// Best-effort Icecast snapshot — fail-open (`None`), never a 5xx.
///
/// A failure is logged and bumps the external-authority error counter
/// (`zukka_music_api_errors_total`, §3.8) exactly like the listener poll.
pub async fn observe_icecast() -> Option<IcecastSnapshot> {
    let url = status_url();
    match fetch_snapshot(&url).await {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            crate::metrics::record_music_api_error();
            tracing::warn!(url, "icecast status observation failed: {err}");
            None
        }
    }
}

/// Fetches and parses the Icecast status document within the 1 s budget.
async fn fetch_snapshot(url: &str) -> Result<IcecastSnapshot, String> {
    let client = reqwest::Client::builder()
        .timeout(ICECAST_TIMEOUT)
        .build()
        .map_err(|err| format!("building HTTP client failed: {err}"))?;
    let body = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("GET {url} failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("GET {url} returned an error: {err}"))?
        .text()
        .await
        .map_err(|err| format!("reading {url} failed: {err}"))?;
    parse_snapshot(&body).ok_or_else(|| format!("malformed JSON from {url}"))
}

#[cfg(test)]
mod tests {
    use super::{
        IcecastSnapshot, ObservedHints, ObservedSource, derive_observed_layer, mount_layer,
        normalize_title, parse_snapshot,
    };
    use crate::dto::{ObservedNowPlaying, SourceLayer};

    const SINGLE: &str = r#"{"icestats":{"source":{"server_name":"zukka",
        "listenurl":"http://icecast:8000/stream.mp3","title":"BistroBoy - Emotions",
        "artist":"BistroBoy","listeners":12,"stream_start":"2026-11-20T10:00:00Z"}}}"#;

    #[test]
    fn parses_single_source_object() {
        let snapshot = parse_snapshot(SINGLE).expect("snapshot");
        assert_eq!(snapshot.sources.len(), 1);
        let source = &snapshot.sources[0];
        assert_eq!(
            source.mount.as_deref(),
            Some("http://icecast:8000/stream.mp3")
        );
        assert_eq!(source.title.as_deref(), Some("BistroBoy - Emotions"));
        assert_eq!(source.artist.as_deref(), Some("BistroBoy"));
        assert_eq!(source.listeners, 12);
        assert_eq!(source.stream_start.as_deref(), Some("2026-11-20T10:00:00Z"));
        assert_eq!(source.server_name.as_deref(), Some("zukka"));
    }

    #[test]
    fn parses_source_array() {
        let body = r#"{"icestats":{"source":[{"mount":"/a.mp3","listeners":3},
            {"mount":"/b.mp3","listeners":4,"title":"T"}]}}"#;
        let snapshot = parse_snapshot(body).expect("snapshot");
        assert_eq!(snapshot.sources.len(), 2);
        assert_eq!(snapshot.sources[0].listeners, 3);
        assert_eq!(snapshot.sources[1].title.as_deref(), Some("T"));
    }

    #[test]
    fn empty_or_missing_source_is_empty_snapshot() {
        for body in [r#"{"icestats":{}}"#, r#"{"icestats":{"source":null}}"#] {
            let snapshot = parse_snapshot(body).expect("snapshot");
            assert!(snapshot.sources.is_empty(), "{body}");
        }
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse_snapshot("not json").is_none());
        assert!(parse_snapshot("<html>").is_none());
    }

    #[test]
    fn missing_title_stays_none() {
        let body = r#"{"icestats":{"source":{"mount":"/stream.mp3","listeners":1}}}"#;
        let snapshot = parse_snapshot(body).expect("snapshot");
        assert!(snapshot.sources[0].title.is_none());
        assert!(snapshot.sources[0].artist.is_none());
        assert_eq!(snapshot.sources[0].mount.as_deref(), Some("/stream.mp3"));
    }

    #[test]
    fn output_mount_has_no_layer() {
        assert_eq!(mount_layer("/stream.mp3"), None);
        assert_eq!(mount_layer("http://icecast:8000/stream.mp3"), None);
    }

    #[test]
    fn normalizes_case_spacing_and_punctuation() {
        assert_eq!(
            normalize_title("  BistroBoy - Emotions! "),
            "bistroboy emotions"
        );
        assert_eq!(normalize_title("BISTROBOY--Emotions"), "bistroboy emotions");
        assert_eq!(normalize_title("???"), "");
    }

    fn snapshot_with(title: Option<&str>, artist: Option<&str>) -> IcecastSnapshot {
        IcecastSnapshot {
            sources: vec![ObservedSource {
                mount: Some("/stream.mp3".to_string()),
                title: title.map(str::to_string),
                artist: artist.map(str::to_string),
                listeners: 5,
                stream_start: None,
                server_name: Some("zukka".to_string()),
            }],
        }
    }

    fn db_now_playing(title: &str, artist: &str) -> ObservedNowPlaying {
        ObservedNowPlaying {
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            source: None,
        }
    }

    #[test]
    fn unreachable_icecast_is_not_on_air() {
        let observed = derive_observed_layer(None, &ObservedHints::default());
        assert!(!observed.on_air);
        assert_eq!(observed.layer, None);
        assert_eq!(observed.reason, "icecast_unreachable");
    }

    #[test]
    fn empty_snapshot_is_silent() {
        let snapshot = IcecastSnapshot::default();
        let observed = derive_observed_layer(Some(&snapshot), &ObservedHints::default());
        assert!(!observed.on_air);
        assert_eq!(observed.layer, None);
        assert_eq!(observed.reason, "silent");
    }

    #[test]
    fn live_wins() {
        let snapshot = snapshot_with(Some("Anything"), None);
        let hints = ObservedHints {
            live_active: true,
            ..ObservedHints::default()
        };
        let observed = derive_observed_layer(Some(&snapshot), &hints);
        assert!(observed.on_air);
        assert_eq!(observed.layer, Some(SourceLayer::Live));
        assert_eq!(observed.reason, "live");
    }

    #[test]
    fn db_match_is_queue_by_default() {
        let snapshot = snapshot_with(Some("Nachtfahrt"), Some("Kranich"));
        let hints = ObservedHints {
            db_now_playing: Some(db_now_playing("Nachtfahrt", "Kranich")),
            running_kind: Some("playlist".to_string()),
            ..ObservedHints::default()
        };
        let observed = derive_observed_layer(Some(&snapshot), &hints);
        assert_eq!(observed.layer, Some(SourceLayer::Queue));
        assert_eq!(observed.reason, "db_match");
    }

    #[test]
    fn db_match_recording_is_show() {
        let snapshot = snapshot_with(Some("Kranich - Nachtfahrt"), None);
        let hints = ObservedHints {
            db_now_playing: Some(db_now_playing("Nachtfahrt", "Kranich")),
            running_kind: Some("recording".to_string()),
            ..ObservedHints::default()
        };
        let observed = derive_observed_layer(Some(&snapshot), &hints);
        assert_eq!(observed.layer, Some(SourceLayer::Show));
    }

    #[test]
    fn shared_title_is_rotation() {
        let snapshot = snapshot_with(Some("Emotions"), Some("BistroBoy"));
        let hints = ObservedHints {
            shared_titles: vec!["Emotions".to_string()],
            ..ObservedHints::default()
        };
        let observed = derive_observed_layer(Some(&snapshot), &hints);
        assert_eq!(observed.layer, Some(SourceLayer::Rotation));
        assert_eq!(observed.reason, "rotation_match");
    }

    #[test]
    fn external_title_is_relay() {
        let snapshot = snapshot_with(Some("SomaFM - Deep Space"), Some("SomaFM"));
        let hints = ObservedHints {
            shared_titles: vec!["Emotions".to_string()],
            ..ObservedHints::default()
        };
        let observed = derive_observed_layer(Some(&snapshot), &hints);
        assert_eq!(observed.layer, Some(SourceLayer::Relay));
        assert_eq!(observed.reason, "external");
        let now = observed.now_playing.expect("now playing");
        assert_eq!(now.title.as_deref(), Some("SomaFM - Deep Space"));
        assert_eq!(now.source.as_deref(), Some("icecast"));
    }

    #[test]
    fn relay_setting_without_metadata() {
        let snapshot = snapshot_with(None, None);
        let hints = ObservedHints {
            relay_active_setting: true,
            ..ObservedHints::default()
        };
        let observed = derive_observed_layer(Some(&snapshot), &hints);
        assert_eq!(observed.layer, Some(SourceLayer::Relay));
        assert_eq!(observed.reason, "relay_setting");
    }

    #[test]
    fn on_air_without_signals_is_unknown() {
        let snapshot = snapshot_with(None, None);
        let observed = derive_observed_layer(Some(&snapshot), &ObservedHints::default());
        assert!(observed.on_air);
        assert_eq!(observed.layer, None);
        assert_eq!(observed.reason, "unknown");
    }
}
