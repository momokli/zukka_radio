//! `ffprobe` integration (Issue #18, US-6; portal.md D6/D7).
//!
//! The probe result decides whether a stream derivate is needed (D7). Parsing
//! is a pure function so it can be unit-tested against stored fixtures; the
//! subprocess call is a thin wrapper.

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

/// One stream as reported by `ffprobe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeStream {
    pub codec_name: Option<String>,
    pub codec_type: Option<String>,
    pub duration_ms: Option<i64>,
    pub bit_rate: Option<i64>,
}

/// Parsed `ffprobe` result (`-show_streams -show_format`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub format_name: Option<String>,
    pub duration_ms: Option<i64>,
    pub bit_rate: Option<i64>,
    pub streams: Vec<ProbeStream>,
}

impl ProbeResult {
    /// `true` when any stream is a video stream (D7 condition b).
    pub fn has_video(&self) -> bool {
        self.streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("video"))
    }

    /// Codec of the first audio stream, if any.
    pub fn primary_audio_codec(&self) -> Option<&str> {
        self.streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("audio"))
            .and_then(|s| s.codec_name.as_deref())
    }
}

/// `ffprobe` failure.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The binary could not be started (missing `FFPROBE_BIN`?).
    #[error("failed to run ffprobe: {0}")]
    Spawn(String),
    /// `ffprobe` exited non-zero.
    #[error("ffprobe exited with status {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    /// The output was not parseable JSON.
    #[error("could not parse ffprobe output: {0}")]
    Parse(String),
}

#[derive(Debug, Deserialize)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Debug, Deserialize)]
struct RawStream {
    codec_name: Option<String>,
    codec_type: Option<String>,
    duration: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    duration: Option<String>,
    bit_rate: Option<String>,
}

/// Seconds (as an `ffprobe` decimal string) → milliseconds.
fn secs_to_ms(raw: Option<&str>) -> Option<i64> {
    let secs: f64 = raw?.trim().parse().ok()?;
    if secs.is_finite() {
        Some((secs * 1000.0).round() as i64)
    } else {
        None
    }
}

fn parse_i64(raw: Option<&str>) -> Option<i64> {
    raw?.trim().parse().ok()
}

/// Parses `ffprobe -print_format json` output. A syntactically invalid document
/// is a [`ProbeError::Parse`].
pub fn parse_ffprobe_json(bytes: &[u8]) -> Result<ProbeResult, ProbeError> {
    let raw: RawProbe =
        serde_json::from_slice(bytes).map_err(|e| ProbeError::Parse(e.to_string()))?;
    let streams = raw
        .streams
        .into_iter()
        .map(|s| ProbeStream {
            codec_name: s.codec_name,
            codec_type: s.codec_type,
            duration_ms: secs_to_ms(s.duration.as_deref()),
            bit_rate: parse_i64(s.bit_rate.as_deref()),
        })
        .collect();
    let (format_name, duration_ms, bit_rate) = match raw.format {
        Some(f) => (
            f.format_name,
            secs_to_ms(f.duration.as_deref()),
            parse_i64(f.bit_rate.as_deref()),
        ),
        None => (None, None, None),
    };
    Ok(ProbeResult {
        format_name,
        duration_ms,
        bit_rate,
        streams,
    })
}

/// Runs `ffprobe` against `path` and parses the result. A non-zero exit or an
/// unparseable document is an error (the job then becomes `failed`).
pub async fn probe(bin: &str, path: &Path) -> Result<ProbeResult, ProbeError> {
    let output = tokio::process::Command::new(bin)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| ProbeError::Spawn(e.to_string()))?;

    if !output.status.success() {
        return Err(ProbeError::Exit {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    parse_ffprobe_json(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_audio_only_stream() {
        let json = br#"{
            "streams": [
                {"codec_name": "flac", "codec_type": "audio", "duration": "12.500000", "bit_rate": "900000"}
            ],
            "format": {"format_name": "flac", "duration": "12.500000", "bit_rate": "900000"}
        }"#;
        let probe = parse_ffprobe_json(json).unwrap();
        assert_eq!(probe.format_name.as_deref(), Some("flac"));
        assert_eq!(probe.duration_ms, Some(12_500));
        assert!(!probe.has_video());
        assert_eq!(probe.primary_audio_codec(), Some("flac"));
    }

    #[test]
    fn detects_video_stream() {
        let json = br#"{
            "streams": [
                {"codec_name": "vp9", "codec_type": "video"},
                {"codec_name": "opus", "codec_type": "audio"}
            ],
            "format": {"format_name": "matroska,webm"}
        }"#;
        let probe = parse_ffprobe_json(json).unwrap();
        assert!(probe.has_video());
        assert_eq!(probe.primary_audio_codec(), Some("opus"));
    }

    #[test]
    fn broken_json_is_parse_error() {
        let err = parse_ffprobe_json(b"{ not json").unwrap_err();
        assert!(matches!(err, ProbeError::Parse(_)));
    }

    #[test]
    fn missing_fields_are_none() {
        let probe = parse_ffprobe_json(br#"{"streams": [], "format": {}}"#).unwrap();
        assert_eq!(probe.duration_ms, None);
        assert!(probe.streams.is_empty());
        assert_eq!(probe.primary_audio_codec(), None);
    }
}
