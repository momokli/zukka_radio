//! Control-bridge wire protocol (Issue #148, `docs/control-api.md` §6).
//!
//! Pure, testable pieces shared by the connection loop and the engine client:
//! frame DTOs, §6.4 error codes, args validation + Liquidsoap command
//! construction (§6.1.1), the §6.2 binary meter encoder and the canonical
//! channel ordering of §5.5.

use serde::{Deserialize, Serialize};

/// Reserved `u16` in the binary meter header (§6.2 offset 2).
pub const METER_HEADER_RESERVED: u16 = 0;
/// Protocol version byte in the binary meter header (§6.2 offset 0).
pub const METER_PROTOCOL_VERSION: u8 = 1;
/// `f32` MIN used to encode "no signal" (`-inf`) in meter frames (§6.2).
pub const METER_NEG_INF: f32 = f32::MIN;
/// Contract cap on the channel count `N` in a binary meter frame (§6.2): `u8`.
pub const METER_MAX_CHANNELS: usize = u8::MAX as usize;

/// Contract version the bridge speaks (§1).
pub const API_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// §6.4 error codes
// ---------------------------------------------------------------------------

/// Contract error codes carried in `error.code` (§6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsErrorCode {
    UnknownCommand,
    InvalidArgs,
    ChannelNotFound,
    BadGain,
    BadSource,
    CommandFailed,
    EngineUnavailable,
    VersionMismatch,
    RateLimited,
    /// The authenticated session lacks `control.write` (§6.4, additive).
    Forbidden,
}

impl WsErrorCode {
    /// The wire name used in `error.code`.
    pub fn as_str(self) -> &'static str {
        match self {
            WsErrorCode::UnknownCommand => "unknown_command",
            WsErrorCode::InvalidArgs => "invalid_args",
            WsErrorCode::ChannelNotFound => "channel_not_found",
            WsErrorCode::BadGain => "bad_gain",
            WsErrorCode::BadSource => "bad_source",
            WsErrorCode::CommandFailed => "command_failed",
            WsErrorCode::EngineUnavailable => "engine_unavailable",
            WsErrorCode::VersionMismatch => "version_mismatch",
            WsErrorCode::RateLimited => "rate_limited",
            WsErrorCode::Forbidden => "forbidden",
        }
    }
}

/// A structured command failure (maps 1:1 onto the `error` object of §6.1).
#[derive(Debug, Clone, PartialEq)]
pub struct WsError {
    pub code: WsErrorCode,
    pub message: String,
    /// Optional `error.details.field`.
    pub field: Option<String>,
}

impl WsError {
    pub fn new(code: WsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field: None,
        }
    }

    pub fn with_field(code: WsErrorCode, message: impl Into<String>, field: &str) -> Self {
        Self {
            code,
            message: message.into(),
            field: Some(field.to_string()),
        }
    }

    /// Serializes to the §6.1 `error` object.
    pub fn to_json(&self) -> serde_json::Value {
        let mut error = serde_json::json!({
            "code": self.code.as_str(),
            "message": self.message,
        });
        if let Some(field) = &self.field {
            error["details"] = serde_json::json!({ "field": field });
        }
        error
    }
}

// ---------------------------------------------------------------------------
// Incoming frames
// ---------------------------------------------------------------------------

/// A client → bridge command frame (§6.1).
#[derive(Debug, Clone, Deserialize)]
pub struct CommandFrame {
    /// Correlation id; echoed back (or `null` when absent).
    #[serde(default)]
    pub id: Option<String>,
    /// Defaults to `1`; any other value → `version_mismatch`.
    #[serde(default)]
    pub api_version: Option<u32>,
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
}

/// The first-frame auth message (§2.2).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthFrame {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub token: Option<String>,
}

/// A validated command, ready to dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send `command` to the Liquidsoap command server.
    Engine(String),
    /// `state.get` → reply with a [`CockpitState`](super::super::api_client::CockpitState).
    State,
    /// `ping` → reply `{pong:true,t}`.
    Ping,
    /// `meters.subscribe` → toggle the meter representation for this connection.
    MetersSubscribe { binary: bool },
}

fn args_object(
    frame: &CommandFrame,
) -> Result<serde_json::Map<String, serde_json::Value>, WsError> {
    match &frame.args {
        Some(serde_json::Value::Object(map)) => Ok(map.clone()),
        None => Ok(serde_json::Map::new()),
        Some(_) => Err(WsError::with_field(
            WsErrorCode::InvalidArgs,
            "args must be an object",
            "args",
        )),
    }
}

fn require_str(
    args: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, WsError> {
    match args.get(field) {
        Some(serde_json::Value::String(value)) => Ok(value.clone()),
        _ => Err(WsError::with_field(
            WsErrorCode::InvalidArgs,
            format!("missing or invalid `{field}`"),
            field,
        )),
    }
}

fn require_bool(
    args: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<bool, WsError> {
    match args.get(field) {
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        _ => Err(WsError::with_field(
            WsErrorCode::InvalidArgs,
            format!("missing or invalid `{field}`"),
            field,
        )),
    }
}

/// `f64` args are accepted as JSON numbers (rejects strings/bools).
fn require_number(
    args: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<f64, WsError> {
    match args.get(field) {
        Some(serde_json::Value::Number(value)) => value.as_f64().ok_or_else(|| {
            WsError::with_field(
                WsErrorCode::InvalidArgs,
                format!("invalid `{field}`"),
                field,
            )
        }),
        _ => Err(WsError::with_field(
            WsErrorCode::InvalidArgs,
            format!("missing or invalid `{field}`"),
            field,
        )),
    }
}

/// Formats a gain so `-6.0` becomes `-6` (matching the engine grammar).
fn format_gain(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Validates and resolves a single argument for a fader-style gain field.
///
/// `gain_db` ∈ `[-60, 12]`, `linear` ∈ `[0, 1]`; the two are mutually exclusive
/// (§6.1.1/§6.4). Exactly one must be present.
fn fader_value(args: &serde_json::Map<String, serde_json::Value>) -> Result<String, WsError> {
    let gain = args.get("gain_db").is_some();
    let linear = args.get("linear").is_some();
    if gain && linear {
        return Err(WsError::with_field(
            WsErrorCode::InvalidArgs,
            "gain_db and linear are mutually exclusive",
            "gain_db",
        ));
    }
    if gain {
        let value = require_number(args, "gain_db")?;
        if !(-60.0..=12.0).contains(&value) {
            return Err(WsError::with_field(
                WsErrorCode::BadGain,
                "gain_db out of range [-60, 12]",
                "gain_db",
            ));
        }
        return Ok(format_gain(value));
    }
    if linear {
        let value = require_number(args, "linear")?;
        if !(0.0..=1.0).contains(&value) {
            return Err(WsError::with_field(
                WsErrorCode::BadGain,
                "linear out of range [0, 1]",
                "linear",
            ));
        }
        return Ok(format_gain(value));
    }
    Err(WsError::with_field(
        WsErrorCode::InvalidArgs,
        "either gain_db or linear is required",
        "gain_db",
    ))
}

/// Validates a `gain_db` field used by `mixer.trim`/`mixer.master`.
fn gain_db(
    args: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, WsError> {
    let value = require_number(args, field)?;
    if !(-60.0..=12.0).contains(&value) {
        return Err(WsError::with_field(
            WsErrorCode::BadGain,
            format!("{field} out of range [-60, 12]"),
            field,
        ));
    }
    Ok(format_gain(value))
}

/// Validates a command frame and resolves it to an [`Action`] (§6.1.1).
pub fn resolve(frame: &CommandFrame) -> Result<Action, WsError> {
    if let Some(version) = frame.api_version {
        if version != API_VERSION {
            return Err(WsError::new(
                WsErrorCode::VersionMismatch,
                format!("unsupported api_version {version}"),
            ));
        }
    }
    let cmd = frame
        .cmd
        .as_deref()
        .ok_or_else(|| WsError::with_field(WsErrorCode::InvalidArgs, "missing `cmd`", "cmd"))?;
    let args = args_object(frame)?;

    let action = match cmd {
        "state.get" => Action::State,
        "ping" => Action::Ping,
        "meters.subscribe" => Action::MetersSubscribe {
            binary: match args.get("binary") {
                Some(serde_json::Value::Bool(value)) => *value,
                None => true,
                _ => {
                    return Err(WsError::with_field(
                        WsErrorCode::InvalidArgs,
                        "binary must be a boolean",
                        "binary",
                    ));
                }
            },
        },
        "mixer.set" => {
            let channel = require_str(&args, "channel")?;
            let value = fader_value(&args)?;
            Action::Engine(format!("mixer.set {channel} {value}"))
        }
        "mixer.trim" => {
            let channel = require_str(&args, "channel")?;
            let value = gain_db(&args, "gain_db")?;
            Action::Engine(format!("mixer.trim {channel} {value}"))
        }
        "mixer.mute" => {
            let channel = require_str(&args, "channel")?;
            let mute = require_bool(&args, "mute")?;
            Action::Engine(format!("mixer.mute {channel} {mute}"))
        }
        "mixer.solo" => {
            let channel = require_str(&args, "channel")?;
            let solo = require_bool(&args, "solo")?;
            Action::Engine(format!("mixer.solo {channel} {solo}"))
        }
        "mixer.exclusive" => {
            let channel = require_str(&args, "channel")?;
            Action::Engine(format!("mixer.exclusive {channel}"))
        }
        "mixer.patch" => {
            let channel = require_str(&args, "channel")?;
            let source = require_str(&args, "source")?;
            Action::Engine(format!("mixer.patch {channel} {source}"))
        }
        "mixer.master" => {
            let value = gain_db(&args, "gain_db")?;
            Action::Engine(format!("mixer.master {value}"))
        }
        "layer.preset" => {
            let preset = require_str(&args, "preset")?;
            Action::Engine(format!("layer.preset {preset}"))
        }
        "layer.force" => {
            let layer = require_str(&args, "layer")?;
            Action::Engine(format!("layer.force {layer}"))
        }
        "layer.enable" => {
            let layer = require_str(&args, "layer")?;
            let enabled = require_bool(&args, "enabled")?;
            Action::Engine(format!("layer.enable {layer} {enabled}"))
        }
        "monitor.enable" => {
            let enabled = require_bool(&args, "enabled")?;
            Action::Engine(format!("monitor.enable {enabled}"))
        }
        "monitor.status" => Action::Engine("monitor.status".to_string()),
        "live.set" => {
            let mount = require_str(&args, "mount")?;
            Action::Engine(format!("live.set {mount}"))
        }
        "live.clear" => Action::Engine("live.clear".to_string()),
        "live.status" => Action::Engine("live.status".to_string()),
        "tape.start" => {
            let tape_id = require_str(&args, "tape_id")?;
            Action::Engine(format!("tape.start {tape_id}"))
        }
        "tape.stop" => Action::Engine("tape.stop".to_string()),
        other => {
            return Err(WsError::new(
                WsErrorCode::UnknownCommand,
                format!("unknown command `{other}`"),
            ));
        }
    };
    Ok(action)
}

// ---------------------------------------------------------------------------
// Engine error parsing (§6.4, analog scheduler/src/liquidsoap.rs)
// ---------------------------------------------------------------------------

/// Classifies an engine response body into a §6.4 code.
///
/// `None` means success (`OK`/JSON). The `ERROR <code>: …` prefix is stable
/// across `mixer.liq`/`control.liq`.
pub fn parse_engine_error(body: &str) -> Option<WsErrorCode> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Some(WsErrorCode::CommandFailed);
    }
    if let Some(rest) = trimmed.strip_prefix("ERROR") {
        let token = rest
            .trim_start()
            .split(|c: char| c == ':' || c.is_whitespace())
            .next()
            .unwrap_or("");
        return Some(match token {
            "unknown_channel" | "unknown_layer" => WsErrorCode::ChannelNotFound,
            "bad_gain" => WsErrorCode::BadGain,
            "bad_source" => WsErrorCode::BadSource,
            "invalid_args" => WsErrorCode::InvalidArgs,
            _ => WsErrorCode::CommandFailed,
        });
    }
    if trimmed == "OK" || trimmed.starts_with('{') || trimmed.starts_with('[') {
        None
    } else {
        Some(WsErrorCode::CommandFailed)
    }
}

// ---------------------------------------------------------------------------
// Meters (§4.5/§6.2)
// ---------------------------------------------------------------------------

/// One channel's meter reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMeter {
    pub id: String,
    #[serde(default)]
    pub rms_l: Option<f32>,
    #[serde(default)]
    pub rms_r: Option<f32>,
    #[serde(default)]
    pub peak: Option<f32>,
}

/// Parses a `mixer.meters` JSON array (§4.5). `null` levels stay `None`.
pub fn parse_meters(body: &str) -> Result<Vec<ChannelMeter>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|err| format!("invalid meters json: {err}"))?;
    let array = value.as_array().ok_or("meters json is not an array")?;
    let mut meters = Vec::with_capacity(array.len());
    for entry in array {
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("meter entry without id")?
            .to_string();
        let number = |key: &str| -> Option<f32> {
            match entry.get(key) {
                Some(serde_json::Value::Number(n)) => n.as_f64().map(|v| v as f32),
                // `-inf` is encoded as JSON `null` (§4.5).
                _ => None,
            }
        };
        meters.push(ChannelMeter {
            id,
            rms_l: number("rms_l"),
            rms_r: number("rms_r"),
            peak: number("peak"),
        });
    }
    Ok(meters)
}

/// The six fixed program channels, in normative §5.5 order.
pub const PROGRAM_CHANNELS: [&str; 6] = ["live", "show", "queue", "rotation", "relay", "fun"];

/// Group rank of a channel id for the normative `channels[]` order (§5.5).
fn channel_rank(id: &str) -> (u8, i64, String) {
    if let Some(index) = PROGRAM_CHANNELS.iter().position(|c| *c == id) {
        return (0, index as i64, id.to_string());
    }
    if let Some(rest) = id.strip_prefix("harbor.slot-") {
        if let Ok(n) = rest.parse::<i64>() {
            return (1, n, id.to_string());
        }
    }
    if let Some(rest) = id.strip_prefix("relay.slot-") {
        if let Ok(n) = rest.parse::<i64>() {
            return (2, n, id.to_string());
        }
    }
    if let Some(rest) = id.strip_prefix("adhoc-") {
        if let Ok(n) = rest.parse::<i64>() {
            return (3, n, id.to_string());
        }
    }
    (4, 0, id.to_string())
}

/// Sorts channel ids into the normative §5.5 order.
pub fn canonical_order<I, S>(ids: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut ids: Vec<String> = ids.into_iter().map(|s| s.as_ref().to_string()).collect();
    ids.sort_by_key(|a| channel_rank(a));
    ids.dedup();
    ids
}

/// Encodes the §6.2 binary meter frame for `meters` **in the given order**.
///
/// Layout: `[u8 ver=1][u8 N][u16 0][f32[3N] LE]`, length `4 + N*12`.
/// `None` levels are encoded as [`METER_NEG_INF`].
///
/// `N` is a `u8`, so at most [`METER_MAX_CHANNELS`] (255) channels are encoded;
/// the order is truncated to the first 255 entries, keeping the header count and
/// the payload length consistent (`4 + N*12`). The target model stays well below
/// this cap (see §6.2).
pub fn encode_meter_frame(order: &[String], meters: &[ChannelMeter]) -> Vec<u8> {
    let count = order.len().min(METER_MAX_CHANNELS);
    let mut frame = Vec::with_capacity(4 + count * 12);
    frame.push(METER_PROTOCOL_VERSION);
    frame.push(count as u8);
    frame.extend_from_slice(&METER_HEADER_RESERVED.to_le_bytes());
    for id in order.iter().take(count) {
        let entry = meters.iter().find(|m| &m.id == id);
        let (rms_l, rms_r, peak) = match entry {
            Some(m) => (
                m.rms_l.unwrap_or(METER_NEG_INF),
                m.rms_r.unwrap_or(METER_NEG_INF),
                m.peak.unwrap_or(METER_NEG_INF),
            ),
            None => (METER_NEG_INF, METER_NEG_INF, METER_NEG_INF),
        };
        frame.extend_from_slice(&rms_l.to_le_bytes());
        frame.extend_from_slice(&rms_r.to_le_bytes());
        frame.extend_from_slice(&peak.to_le_bytes());
    }
    frame
}

/// Builds the `meters` JSON event `data` for `binary:false` clients (§6.3).
pub fn meters_json(order: &[String], meters: &[ChannelMeter]) -> serde_json::Value {
    let frames: Vec<serde_json::Value> = order
        .iter()
        .map(|id| match meters.iter().find(|m| &m.id == id) {
            Some(m) => serde_json::json!({
                "id": id,
                "rms_l": m.rms_l,
                "rms_r": m.rms_r,
                "peak": m.peak,
            }),
            None => serde_json::json!({
                "id": id,
                "rms_l": null,
                "rms_r": null,
                "peak": null,
            }),
        })
        .collect();
    serde_json::json!({ "frames": frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cmd: &str, args: serde_json::Value) -> CommandFrame {
        CommandFrame {
            id: Some("c-1".into()),
            api_version: None,
            cmd: Some(cmd.into()),
            args: Some(args),
        }
    }

    #[test]
    fn resolve_builds_expected_engine_commands() {
        assert_eq!(
            resolve(&frame(
                "mixer.set",
                serde_json::json!({"channel":"live","gain_db":-6.0})
            )),
            Ok(Action::Engine("mixer.set live -6".into()))
        );
        assert_eq!(
            resolve(&frame(
                "mixer.set",
                serde_json::json!({"channel":"live","linear":0.5})
            )),
            Ok(Action::Engine("mixer.set live 0.5".into()))
        );
        assert_eq!(
            resolve(&frame(
                "mixer.mute",
                serde_json::json!({"channel":"live","mute":true})
            )),
            Ok(Action::Engine("mixer.mute live true".into()))
        );
        assert_eq!(
            resolve(&frame(
                "mixer.exclusive",
                serde_json::json!({"channel":"queue"})
            )),
            Ok(Action::Engine("mixer.exclusive queue".into()))
        );
        assert_eq!(
            resolve(&frame(
                "live.set",
                serde_json::json!({"mount":"/live/slot-01"})
            )),
            Ok(Action::Engine("live.set /live/slot-01".into()))
        );
        assert_eq!(
            resolve(&frame("ping", serde_json::json!({}))),
            Ok(Action::Ping)
        );
        assert_eq!(
            resolve(&frame("state.get", serde_json::json!({}))),
            Ok(Action::State)
        );
    }

    #[test]
    fn gain_ranges_are_enforced() {
        let err = resolve(&frame(
            "mixer.set",
            serde_json::json!({"channel":"live","gain_db":99.0}),
        ))
        .unwrap_err();
        assert_eq!(err.code, WsErrorCode::BadGain);
        assert_eq!(err.field.as_deref(), Some("gain_db"));

        let err = resolve(&frame(
            "mixer.set",
            serde_json::json!({"channel":"live","linear":1.5}),
        ))
        .unwrap_err();
        assert_eq!(err.code, WsErrorCode::BadGain);

        let err = resolve(&frame(
            "mixer.trim",
            serde_json::json!({"channel":"live","gain_db":-61.0}),
        ))
        .unwrap_err();
        assert_eq!(err.code, WsErrorCode::BadGain);
    }

    #[test]
    fn gain_and_linear_are_exclusive() {
        let err = resolve(&frame(
            "mixer.set",
            serde_json::json!({"channel":"live","gain_db":-6.0,"linear":0.5}),
        ))
        .unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
    }

    #[test]
    fn version_mismatch_and_unknown_command() {
        let mut f = frame("ping", serde_json::json!({}));
        f.api_version = Some(2);
        assert_eq!(resolve(&f).unwrap_err().code, WsErrorCode::VersionMismatch);

        assert_eq!(
            resolve(&frame("nope", serde_json::json!({})))
                .unwrap_err()
                .code,
            WsErrorCode::UnknownCommand
        );
    }

    #[test]
    fn meters_subscribe_parses_binary_flag() {
        assert_eq!(
            resolve(&frame(
                "meters.subscribe",
                serde_json::json!({"binary": false})
            )),
            Ok(Action::MetersSubscribe { binary: false })
        );
        assert_eq!(
            resolve(&frame("meters.subscribe", serde_json::json!({}))),
            Ok(Action::MetersSubscribe { binary: true })
        );
    }

    #[test]
    fn parse_engine_error_maps_codes() {
        assert_eq!(parse_engine_error("OK"), None);
        assert_eq!(
            parse_engine_error("ERROR bad_gain: x"),
            Some(WsErrorCode::BadGain)
        );
        assert_eq!(
            parse_engine_error("ERROR unknown_channel: x"),
            Some(WsErrorCode::ChannelNotFound)
        );
        assert_eq!(
            parse_engine_error("garbage"),
            Some(WsErrorCode::CommandFailed)
        );
    }

    #[test]
    fn canonical_order_follows_spec_groups() {
        let order = canonical_order([
            "adhoc-2",
            "fun",
            "relay.slot-01",
            "live",
            "harbor.slot-03",
            "queue",
            "adhoc-1",
        ]);
        assert_eq!(
            order,
            vec![
                "live",
                "queue",
                "fun",
                "harbor.slot-03",
                "relay.slot-01",
                "adhoc-1",
                "adhoc-2",
            ]
        );
    }

    #[test]
    fn meter_frame_layout_and_length() {
        let meters = vec![
            ChannelMeter {
                id: "live".into(),
                rms_l: Some(-18.2),
                rms_r: Some(-18.0),
                peak: Some(-3.1),
            },
            ChannelMeter {
                id: "show".into(),
                rms_l: None,
                rms_r: None,
                peak: None,
            },
        ];
        let order = vec!["live".to_string(), "show".to_string()];
        let frame = encode_meter_frame(&order, &meters);
        assert_eq!(frame.len(), 4 + 2 * 12);
        assert_eq!(frame[0], 1);
        assert_eq!(frame[1], 2);
        assert_eq!(&frame[2..4], &[0, 0]);
        let rms_l = f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
        assert!((rms_l - (-18.2)).abs() < 1e-4);
        let show_peak = f32::from_le_bytes([
            frame[4 + 12 + 8],
            frame[5 + 12 + 8],
            frame[6 + 12 + 8],
            frame[7 + 12 + 8],
        ]);
        assert_eq!(show_peak, METER_NEG_INF);
    }

    #[test]
    fn meter_frame_caps_channel_count_at_255() {
        // More channels than the `u8` header can express: the frame must keep a
        // consistent length (`4 + N*12`) with `N = 255`.
        let order: Vec<String> = (0..300).map(|i| format!("adhoc-{i}")).collect();
        let meters = vec![];
        let frame = encode_meter_frame(&order, &meters);
        assert_eq!(frame[0], METER_PROTOCOL_VERSION);
        assert_eq!(frame[1], 255);
        assert_eq!(frame.len(), 4 + METER_MAX_CHANNELS * 12);
        assert_eq!(frame.len(), 4 + 255 * 12);
    }

    #[test]
    fn forbidden_code_serializes() {
        let err = WsError::new(WsErrorCode::Forbidden, "missing control.write");
        assert_eq!(err.to_json()["code"], "forbidden");
    }

    #[test]
    fn parse_meters_reads_null_as_none() {
        let body = r#"[{"id":"live","rms_l":-18.2,"rms_r":null,"peak":-3.1}]"#;
        let meters = parse_meters(body).unwrap();
        assert_eq!(meters.len(), 1);
        assert_eq!(meters[0].rms_r, None);
        assert_eq!(meters[0].rms_l, Some(-18.2));
    }

    #[test]
    fn resolve_builds_remaining_engine_commands() {
        let cases: &[(&str, serde_json::Value, &str)] = &[
            (
                "mixer.trim",
                serde_json::json!({"channel":"live","gain_db":-3.0}),
                "mixer.trim live -3",
            ),
            (
                "mixer.solo",
                serde_json::json!({"channel":"relay","solo":true}),
                "mixer.solo relay true",
            ),
            (
                "mixer.patch",
                serde_json::json!({"channel":"fun","source":"file:/x"}),
                "mixer.patch fun file:/x",
            ),
            (
                "mixer.master",
                serde_json::json!({"gain_db":0.0}),
                "mixer.master 0",
            ),
            (
                "layer.preset",
                serde_json::json!({"preset":"live"}),
                "layer.preset live",
            ),
            (
                "layer.force",
                serde_json::json!({"layer":"queue"}),
                "layer.force queue",
            ),
            (
                "layer.enable",
                serde_json::json!({"layer":"queue","enabled":true}),
                "layer.enable queue true",
            ),
            (
                "tape.start",
                serde_json::json!({"tape_id":"t-1"}),
                "tape.start t-1",
            ),
            ("tape.stop", serde_json::json!({}), "tape.stop"),
            ("live.clear", serde_json::json!({}), "live.clear"),
            ("live.status", serde_json::json!({}), "live.status"),
            (
                "monitor.enable",
                serde_json::json!({"enabled":true}),
                "monitor.enable true",
            ),
            ("monitor.status", serde_json::json!({}), "monitor.status"),
        ];
        for (cmd, args, expected) in cases {
            assert_eq!(
                resolve(&frame(cmd, args.clone())),
                Ok(Action::Engine((*expected).into())),
                "{cmd}"
            );
        }
    }

    #[test]
    fn resolve_rejects_missing_or_bad_args() {
        // Missing `source`.
        let err =
            resolve(&frame("mixer.patch", serde_json::json!({"channel":"live"}))).unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("source"));

        // Missing / non-bool `enabled`.
        let err =
            resolve(&frame("layer.enable", serde_json::json!({"layer":"queue"}))).unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("enabled"));
        let err = resolve(&frame(
            "layer.enable",
            serde_json::json!({"layer":"queue","enabled":"yes"}),
        ))
        .unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("enabled"));

        // Non-bool `mute` / missing `solo`.
        let err = resolve(&frame(
            "mixer.mute",
            serde_json::json!({"channel":"live","mute":"yes"}),
        ))
        .unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("mute"));
        let err = resolve(&frame("mixer.solo", serde_json::json!({"channel":"live"}))).unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("solo"));

        // Missing `gain_db` for `mixer.trim`.
        let err = resolve(&frame("mixer.trim", serde_json::json!({"channel":"live"}))).unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("gain_db"));
    }

    #[test]
    fn resolve_rejects_non_object_args() {
        // `args` present but not an object.
        let err = resolve(&frame("mixer.set", serde_json::json!(5))).unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("args"));

        // `cmd` absent entirely.
        let mut f = frame("ping", serde_json::json!({}));
        f.cmd = None;
        let err = resolve(&f).unwrap_err();
        assert_eq!(err.code, WsErrorCode::InvalidArgs);
        assert_eq!(err.field.as_deref(), Some("cmd"));
    }
}
