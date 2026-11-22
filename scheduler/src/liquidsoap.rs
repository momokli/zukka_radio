//! Outbound control channel: scheduler → Liquidsoap (issue #32).
//!
//! The Liquidsoap command server is started by `server.harbor` on port 8010
//! (internal `back` network only, §2 D14). It speaks "telnet over HTTP": a
//! command is sent as the **body** of a `POST` to `/control`, and the response
//! body is the command's return value. This was verified against the pinned
//! `savonet/liquidsoap:v2.4.5` image:
//!
//! ```text
//! POST http://liquidsoap:8010/control
//! live.set /live/slot-03
//! ```
//!
//! Configuration is explicit: when `ZUKKA_LIQUIDSOAP_CONTROL_URL` is unset the
//! push is skipped, which keeps unit/integration tests hermetic. The deployed
//! stacks set it in `env/*.env` (`http://liquidsoap:8010/control`).

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

/// Environment variable holding the full control endpoint URL.
pub const CONTROL_URL_ENV: &str = "ZUKKA_LIQUIDSOAP_CONTROL_URL";

/// `live.set <mount>` — force this caster mount on air (§4.4).
pub fn live_set(mount: &str) -> String {
    format!("live.set {}", mount.trim())
}

/// `live.clear` — drop the override and return to FCFS (§4.4).
pub const LIVE_CLEAR: &str = "live.clear";

/// `live.status` — JSON with `live_order`, `override`, `current` (§4.4).
pub const LIVE_STATUS: &str = "live.status";

/// `tape.start <tape_id>` — open the per-broadcast recorder (§4.4, issue #108).
pub fn tape_start(tape_id: &str) -> String {
    format!("tape.start {}", tape_id.trim())
}

/// `tape.stop` — close the per-broadcast recorder (§4.4, issue #108).
pub const TAPE_STOP: &str = "tape.stop";

/// `tape.status` — JSON `{active, tape_id, path}` (§4.4, issue #108).
pub const TAPE_STATUS: &str = "tape.status";

// ---------------------------------------------------------------------------
// Issue #147: mixer / layer / monitor command builders (§4.1/§4.2/§4.3)
// ---------------------------------------------------------------------------

/// `mixer.set <ch> <gain>` — a fader value (§4.1). `gain` is a bare dB number
/// or a linear factor; the caller validates the range (Q7).
pub fn mixer_set(channel: &str, gain: &str) -> String {
    format!("mixer.set {} {}", channel.trim(), gain.trim())
}

/// `mixer.trim <ch> <gain_db>` — the pre-fader trim (§4.1).
pub fn mixer_trim(channel: &str, gain_db: &str) -> String {
    format!("mixer.trim {} {}", channel.trim(), gain_db.trim())
}

/// `mixer.mute <ch> <true|false>` (§4.1).
pub fn mixer_mute(channel: &str, muted: bool) -> String {
    format!("mixer.mute {} {muted}", channel.trim())
}

/// `mixer.solo <ch> <true|false>` — route the channel to the cue bus (§4.1).
pub fn mixer_solo(channel: &str, solo: bool) -> String {
    format!("mixer.solo {} {solo}", channel.trim())
}

/// `mixer.exclusive <ch>` — unmute only this channel (§4.1).
pub fn mixer_exclusive(channel: &str) -> String {
    format!("mixer.exclusive {}", channel.trim())
}

/// `mixer.patch <ch> <source>` — re-source a channel (§4.1).
pub fn mixer_patch(channel: &str, source: &str) -> String {
    format!("mixer.patch {} {}", channel.trim(), source.trim())
}

/// `mixer.master <gain_db>` — the master fader (§4.1).
pub fn mixer_master(gain_db: &str) -> String {
    format!("mixer.master {}", gain_db.trim())
}

/// `layer.preset <name>` — apply a documented mixer preset (§4.2).
pub fn layer_preset(name: &str) -> String {
    format!("layer.preset {}", name.trim())
}

/// `layer.force <layer|none>` — the manual automation override (§4.2).
pub fn layer_force(layer: &str) -> String {
    format!("layer.force {}", layer.trim())
}

/// `layer.enable <layer> <true|false>` — per-layer availability (§4.2).
pub fn layer_enable(layer: &str, enabled: bool) -> String {
    format!("layer.enable {} {enabled}", layer.trim())
}

/// `monitor.enable <true|false>` (§4.3).
pub fn monitor_enable(enabled: bool) -> String {
    format!("monitor.enable {enabled}")
}

/// `mixer.meters` — per-channel levels as JSON (§4.5).
pub const MIXER_METERS: &str = "mixer.meters";

/// A stable error code parsed from the Liquidsoap command response (§6.4).
///
/// The engine returns `"ERROR <code>: …"` **in the body** (not as an HTTP
/// status); [`parse_engine_error`] maps it to the contract code the scheduler
/// proxy exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineErrorCode {
    /// `unknown_channel` / `unknown_layer` → `404 channel_not_found`.
    ChannelNotFound,
    /// `bad_gain` → `422 bad_gain`.
    BadGain,
    /// `bad_source` → `422 bad_source`.
    BadSource,
    /// `invalid_args` → `422 invalid_args`.
    InvalidArgs,
    /// Any other `ERROR …` / malformed body → `502 command_failed`.
    CommandFailed,
}

impl EngineErrorCode {
    /// The contract code exposed on the wire (§3.1/§6.4).
    pub fn as_str(self) -> &'static str {
        match self {
            EngineErrorCode::ChannelNotFound => "channel_not_found",
            EngineErrorCode::BadGain => "bad_gain",
            EngineErrorCode::BadSource => "bad_source",
            EngineErrorCode::InvalidArgs => "invalid_args",
            EngineErrorCode::CommandFailed => "command_failed",
        }
    }
}

/// Classifies a command response body.
///
/// `None` means success (`OK`/JSON body). `Some(code)` means the engine (or a
/// malformed/empty body) reported a failure. The `ERROR <code>: …` prefix is
/// stable across `mixer.liq`/`control.liq`.
pub fn parse_engine_error(body: &str) -> Option<EngineErrorCode> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Some(EngineErrorCode::CommandFailed);
    }
    if let Some(rest) = trimmed.strip_prefix("ERROR") {
        let token = rest
            .trim_start()
            .split(|c: char| c == ':' || c.is_whitespace())
            .next()
            .unwrap_or("");
        return Some(match token {
            "unknown_channel" | "unknown_layer" => EngineErrorCode::ChannelNotFound,
            "bad_gain" => EngineErrorCode::BadGain,
            "bad_source" => EngineErrorCode::BadSource,
            "invalid_args" => EngineErrorCode::InvalidArgs,
            _ => EngineErrorCode::CommandFailed,
        });
    }
    // A success body is `OK` or a JSON document (§4.1/§4.4).
    if trimmed == "OK" || trimmed.starts_with('{') || trimmed.starts_with('[') {
        None
    } else {
        Some(EngineErrorCode::CommandFailed)
    }
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("reqwest client")
    })
}

fn configured_url() -> Option<String> {
    std::env::var(CONTROL_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Sends one command to `url`, returning the response body.
pub async fn send_to(url: &str, command: &str) -> Result<String, String> {
    let response = client()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body(command.to_string())
        .send()
        .await
        .map_err(|err| format!("POST {url} failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| format!("reading response from {url} failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("POST {url} returned {status}: {}", body.trim()));
    }
    Ok(body)
}

/// Best-effort push through the configured endpoint.
///
/// `Ok(None)` means no endpoint is configured (push disabled). Every caller in
/// the control store persists state first, so a failed push never loses the
/// operator's decision — it is re-asserted on the next scheduler start.
pub async fn push(command: &str) -> Result<Option<String>, String> {
    match configured_url() {
        Some(url) => send_to(&url, command).await.map(Some),
        None => Ok(None),
    }
}

/// Reads back the current Liquidsoap live selection (`live.status`).
pub async fn status() -> Result<Option<String>, String> {
    push(LIVE_STATUS).await
}

/// Read helper for a single command (`read` semantics, Issue #147).
///
/// `Ok(None)` means no endpoint is configured (push disabled) — used by the
/// fail-open state projection (e.g. `mixer.meters`). Transport errors are
/// surfaced so callers can decide how to degrade.
pub async fn read(command: &str) -> Result<Option<String>, String> {
    push(command).await
}

/// Best-effort push with logging; failures are never fatal.
///
/// Shared by the control store and the broadcast transition wiring: a push
/// never loses persisted state, so an unreachable Liquidsoap is only a warning.
pub async fn push_best_effort(command: &str) {
    match push(command).await {
        Ok(Some(body)) => tracing::debug!(command, body = body.trim(), "liquidsoap control ok"),
        Ok(None) => tracing::debug!(command, "liquidsoap control push disabled (no URL)"),
        Err(err) => tracing::warn!(command, "liquidsoap control push failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tape_start_trims_and_prefixes() {
        assert_eq!(
            tape_start("  0192f0a1-0001-7000-8000-000000000003 "),
            "tape.start 0192f0a1-0001-7000-8000-000000000003"
        );
    }

    #[test]
    fn tape_command_constants_are_exact() {
        assert_eq!(TAPE_STOP, "tape.stop");
        assert_eq!(TAPE_STATUS, "tape.status");
    }

    #[test]
    fn mixer_builders_trim_and_prefix() {
        assert_eq!(mixer_set(" live ", " -6.0 "), "mixer.set live -6.0");
        assert_eq!(mixer_trim("live", "-3"), "mixer.trim live -3");
        assert_eq!(mixer_mute(" live", true), "mixer.mute live true");
        assert_eq!(mixer_solo("live", false), "mixer.solo live false");
        assert_eq!(mixer_exclusive(" queue "), "mixer.exclusive queue");
        assert_eq!(mixer_patch("live", " file:/x "), "mixer.patch live file:/x");
        assert_eq!(mixer_master(" 0.0 "), "mixer.master 0.0");
        assert_eq!(layer_preset(" live "), "layer.preset live");
        assert_eq!(layer_force("none"), "layer.force none");
        assert_eq!(layer_enable(" queue ", true), "layer.enable queue true");
        assert_eq!(monitor_enable(false), "monitor.enable false");
        assert_eq!(MIXER_METERS, "mixer.meters");
    }

    #[test]
    fn parse_engine_error_ok_is_none() {
        assert_eq!(parse_engine_error("OK"), None);
        assert_eq!(parse_engine_error("  OK  "), None);
        assert_eq!(parse_engine_error("{\"live_order\":[]}"), None);
        assert_eq!(parse_engine_error("[1,2]"), None);
    }

    #[test]
    fn parse_engine_error_maps_stable_codes() {
        assert_eq!(
            parse_engine_error("ERROR bad_gain: -99"),
            Some(EngineErrorCode::BadGain)
        );
        assert_eq!(
            parse_engine_error("ERROR unknown_channel: x"),
            Some(EngineErrorCode::ChannelNotFound)
        );
        assert_eq!(
            parse_engine_error("ERROR unknown_layer: x"),
            Some(EngineErrorCode::ChannelNotFound)
        );
        assert_eq!(
            parse_engine_error("ERROR bad_source: ftp:x"),
            Some(EngineErrorCode::BadSource)
        );
        assert_eq!(
            parse_engine_error("ERROR invalid_args: usage"),
            Some(EngineErrorCode::InvalidArgs)
        );
    }

    #[test]
    fn parse_engine_error_garbage_is_command_failed() {
        assert_eq!(parse_engine_error(""), Some(EngineErrorCode::CommandFailed));
        assert_eq!(
            parse_engine_error("   "),
            Some(EngineErrorCode::CommandFailed)
        );
        assert_eq!(
            parse_engine_error("ERROR we-failed"),
            Some(EngineErrorCode::CommandFailed)
        );
        assert_eq!(
            parse_engine_error("totally not a response"),
            Some(EngineErrorCode::CommandFailed)
        );
    }

    #[test]
    fn engine_error_code_wire_names() {
        assert_eq!(
            EngineErrorCode::ChannelNotFound.as_str(),
            "channel_not_found"
        );
        assert_eq!(EngineErrorCode::BadGain.as_str(), "bad_gain");
        assert_eq!(EngineErrorCode::BadSource.as_str(), "bad_source");
        assert_eq!(EngineErrorCode::InvalidArgs.as_str(), "invalid_args");
        assert_eq!(EngineErrorCode::CommandFailed.as_str(), "command_failed");
    }
}
