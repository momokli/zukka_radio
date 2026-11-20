//! Waveform core: RMS bars, ZKWV encoder, ffprobe/ffmpeg argument builders
//! (Issue #25, M3; `docs/tapes.md` D7/§3.6, consolidated §3.2).
//!
//! Everything that does not touch a process is a pure function so it is
//! deterministically testable: `bars_for`, `rms_to_byte`, [`RmsBarEncoder`],
//! [`encode_zkwv`] and the `ffprobe_args`/`ffmpeg_args` builders. Process I/O
//! (`probe_media`, [`decode_command`]) is a thin wrapper around those.

use std::collections::VecDeque;
use std::time::Duration;

use serde::Deserialize;

/// Read size for the streaming decoder (64 KiB).
pub const CHUNK_SIZE: usize = 64 * 1024;
/// Decoded sample rate: mono, 8 kHz.
pub const SAMPLE_RATE: u32 = 8000;
/// One bar covers 500 ms (no hop, D7).
pub const MS_PER_BAR: i64 = 500;
/// 500 ms at 8 kHz.
pub const SAMPLES_PER_BAR: usize = 4000;
/// Waveform scheme / ZKWV version.
pub const SCHEME: i64 = 1;
/// Algorithm tag stored in `waveforms.algorithm`.
pub const ALGORITHM: &str = "ffmpeg-rms-500ms/1";
/// Upper guard: 24 h at 500 ms/bar.
pub const MAX_BARS: i64 = 172_800;

/// Number of 500 ms bars for a duration: `ceil(duration_ms / 500)`.
pub fn bars_for(duration_ms: i64) -> i64 {
    if duration_ms <= 0 {
        0
    } else {
        (duration_ms + MS_PER_BAR - 1) / MS_PER_BAR
    }
}

/// Maps a normalized RMS value (0..1) to a dBFS byte:
/// `db = 20*log10(rms)`, `v = clamp(round(255*(db+60)/60), 0, 255)`.
///
/// Silence (`rms <= 0`) → `0`; full scale (`rms >= 1`) → `255`.
pub fn rms_to_byte(rms: f64) -> u8 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0;
    }
    let db = 20.0 * rms.log10();
    let value = (255.0 * (db + 60.0) / 60.0).round();
    value.clamp(0.0, 255.0) as u8
}

/// Streaming RMS-bar encoder.
///
/// Accepts arbitrary `s16le` byte chunks, keeps a carry byte for odd chunk
/// boundaries and a partial window. The produced bytes are independent of the
/// chunking. An incomplete final window is closed with its actual sample count
/// (never zero-padded).
#[derive(Debug, Default)]
pub struct RmsBarEncoder {
    window: VecDeque<i16>,
    carry: Option<u8>,
}

impl RmsBarEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes one byte chunk and returns every bar completed by it.
    pub fn push_chunk(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if bytes.is_empty() {
            return out;
        }

        let mut start = 0usize;
        if let Some(high) = self.carry.take() {
            self.window.push_back(i16::from_le_bytes([high, bytes[0]]));
            start = 1;
        }

        let rest = &bytes[start..];
        let paired = rest.len() / 2 * 2;
        for pair in rest[..paired].chunks_exact(2) {
            self.window
                .push_back(i16::from_le_bytes([pair[0], pair[1]]));
        }
        if paired < rest.len() {
            self.carry = Some(rest[paired]);
        }

        while self.window.len() >= SAMPLES_PER_BAR {
            let samples: Vec<i16> = self.window.drain(..SAMPLES_PER_BAR).collect();
            out.push(window_rms_to_byte(&samples));
        }
        out
    }

    /// Closes the final (partial) window. A trailing unpaired byte is discarded.
    pub fn finish(&mut self) -> Vec<u8> {
        self.carry = None;
        if self.window.is_empty() {
            return Vec::new();
        }
        let samples: Vec<i16> = self.window.drain(..).collect();
        vec![window_rms_to_byte(&samples)]
    }
}

/// RMS of a window in `[-1, 1]` mapped to a byte.
fn window_rms_to_byte(samples: &[i16]) -> u8 {
    if samples.is_empty() {
        return 0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|sample| {
            let value = f64::from(*sample);
            value * value
        })
        .sum();
    let rms = (sum_sq / samples.len() as f64).sqrt() / 32768.0;
    rms_to_byte(rms)
}

/// Encodes a ZKWV frame:
/// `"ZKWV" | version u8=1 | flags u8=0 | reserved u16 BE=0 | bars u32 LE`,
/// followed by exactly `bars` peak bytes.
pub fn encode_zkwv(bars: u32, data: &[u8]) -> Vec<u8> {
    assert_eq!(
        data.len(),
        bars as usize,
        "waveform data length must equal bars"
    );
    let mut out = Vec::with_capacity(12 + data.len());
    out.extend_from_slice(b"ZKWV");
    out.push(1);
    out.push(0);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&bars.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// ffprobe output subset we consume.
#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    format_name: Option<String>,
}

/// Duration + container metadata of a media object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub duration_ms: i64,
    pub format: String,
    pub mime: String,
}

/// Error raised by the waveform pipeline.
#[derive(Debug, thiserror::Error)]
pub enum WaveformError {
    #[error("ffprobe failed: {0}")]
    Probe(String),
    #[error("ffmpeg failed: {0}")]
    Decode(String),
    #[error("invalid media duration: {0}")]
    InvalidDuration(String),
    #[error("waveform too long: {0} bars (max {MAX_BARS})")]
    TooLong(i64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Normalizes ffprobe's `format_name` into `(extension, mime)`.
pub fn normalize_format(format_name: &str) -> (String, String) {
    let name = format_name.to_ascii_lowercase();
    if name.contains("mp3") {
        ("mp3".into(), "audio/mpeg".into())
    } else if name.contains("flac") {
        ("flac".into(), "audio/flac".into())
    } else if name.contains("opus") {
        ("opus".into(), "audio/ogg".into())
    } else if name.contains("ogg") || name.contains("vorbis") {
        ("ogg".into(), "audio/ogg".into())
    } else if name.contains("wav") {
        ("wav".into(), "audio/wav".into())
    } else if name.contains("aac") {
        ("aac".into(), "audio/aac".into())
    } else if name.contains("mp4") || name.contains("m4a") || name.contains("mov") {
        ("m4a".into(), "audio/mp4".into())
    } else {
        let first = name
            .split(',')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("bin");
        (first.to_string(), "application/octet-stream".to_string())
    }
}

/// Authorization header value, or `None` when no token is configured.
pub fn auth_header(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(format!("Authorization: Bearer {token}"))
    }
}

/// `ffprobe` arguments (without the binary). Header option precedes the input.
pub fn ffprobe_args(url: &str, auth: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-show_entries".into(),
        "format=duration,format_name".into(),
        "-of".into(),
        "json".into(),
    ];
    if let Some(header) = auth {
        args.push("-headers".into());
        args.push(header.into());
    }
    args.push(url.into());
    args
}

/// `ffmpeg` decoding arguments: mono 8 kHz signed 16-bit little-endian to
/// stdout, no container.
pub fn ffmpeg_args(url: &str, auth: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-v".into(), "error".into()];
    if let Some(header) = auth {
        args.push("-headers".into());
        args.push(header.into());
    }
    args.extend([
        "-i".into(),
        url.into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        SAMPLE_RATE.to_string(),
        "-f".into(),
        "s16le".into(),
        "-".into(),
    ]);
    args
}

/// Runs ffprobe and parses duration + format.
pub async fn probe_media(
    bin: &str,
    url: &str,
    auth: Option<&str>,
    timeout: Duration,
) -> Result<ProbeResult, WaveformError> {
    let mut command = tokio::process::Command::new(bin);
    command
        .args(ffprobe_args(url, auth))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = command.spawn()?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(WaveformError::Io(err)),
        Err(_) => return Err(WaveformError::Probe("ffprobe timed out".into())),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WaveformError::Probe(format!(
            "exit {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }

    parse_ffprobe_output(&output.stdout)
}

fn parse_ffprobe_output(bytes: &[u8]) -> Result<ProbeResult, WaveformError> {
    let parsed: FfprobeOutput = serde_json::from_slice(bytes)
        .map_err(|err| WaveformError::Probe(format!("invalid ffprobe JSON: {err}")))?;
    let format = parsed
        .format
        .ok_or_else(|| WaveformError::Probe("ffprobe reported no format".into()))?;
    let raw_duration = format
        .duration
        .ok_or_else(|| WaveformError::InvalidDuration("missing format.duration".into()))?;
    let seconds: f64 = raw_duration
        .trim()
        .parse()
        .map_err(|_| WaveformError::InvalidDuration(raw_duration.clone()))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(WaveformError::InvalidDuration(raw_duration));
    }
    let (ext, mime) = normalize_format(format.format_name.as_deref().unwrap_or(""));
    Ok(ProbeResult {
        duration_ms: (seconds * 1000.0).round() as i64,
        format: ext,
        mime,
    })
}

/// Builds the ffmpeg decode command (stdout = `s16le` stream, no buffering).
pub fn decode_command(bin: &str, url: &str, auth: Option<&str>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(bin);
    command
        .args(ffmpeg_args(url, auth))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_all(chunks: &[&[u8]]) -> Vec<u8> {
        let mut encoder = RmsBarEncoder::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(encoder.push_chunk(chunk));
        }
        out.extend(encoder.finish());
        out
    }

    fn samples_bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn bars_for_boundaries() {
        assert_eq!(bars_for(7_194_000), 14_388);
        assert_eq!(bars_for(0), 0);
        assert_eq!(bars_for(-5), 0);
        assert_eq!(bars_for(1), 1);
        assert_eq!(bars_for(499), 1);
        assert_eq!(bars_for(500), 1);
        assert_eq!(bars_for(501), 2);
    }

    #[test]
    fn rms_mapping_extremes() {
        assert_eq!(rms_to_byte(0.0), 0);
        assert_eq!(rms_to_byte(-1.0), 0);
        assert_eq!(rms_to_byte(f64::NAN), 0);
        assert_eq!(rms_to_byte(1.0), 255);
        assert_eq!(rms_to_byte(1e-3), 0, "-60 dBFS is the floor");
        assert_eq!(rms_to_byte(10f64.powf(-1.5)), 128, "-30 dBFS");
    }

    #[test]
    fn rms_mapping_is_monotone() {
        let mut previous = 0;
        for step in 0..=100 {
            let rms = step as f64 / 100.0;
            let value = rms_to_byte(rms);
            assert!(value >= previous, "rms {rms}");
            previous = value;
        }
    }

    #[test]
    fn silence_encodes_to_zero_bytes() {
        let samples = vec![0i16; SAMPLES_PER_BAR * 2];
        let out = encode_all(&[&samples_bytes(&samples)]);
        assert_eq!(out, vec![0, 0]);
    }

    #[test]
    fn full_scale_encodes_to_255() {
        let samples = vec![i16::MAX; SAMPLES_PER_BAR];
        let out = encode_all(&[&samples_bytes(&samples)]);
        assert_eq!(out, vec![255]);
    }

    #[test]
    fn chunking_is_irrelevant_even_for_odd_lengths() {
        let samples: Vec<i16> = (0..SAMPLES_PER_BAR * 2 + 123)
            .map(|index| ((index as i32 % 5000) - 2500) as i16)
            .collect();
        let bytes = samples_bytes(&samples);

        let reference = encode_all(&[&bytes]);
        for chunk_size in [1usize, 7, 4096, CHUNK_SIZE] {
            let chunks: Vec<&[u8]> = bytes.chunks(chunk_size).collect();
            assert_eq!(
                encode_all(&chunks),
                reference,
                "chunk size {chunk_size} must not change the result"
            );
        }

        // An odd trailing byte is discarded, not misaligned.
        let mut odd = bytes.clone();
        odd.push(0x7F);
        assert_eq!(encode_all(&[&odd]), reference);
    }

    #[test]
    fn bar_count_matches_sample_windows() {
        // 7_194_000 ms at 8 kHz = 57_552_000 samples = 14388 windows.
        let samples = 7_194_000i64 * 8;
        assert_eq!(samples / SAMPLES_PER_BAR as i64, 14_388);
        assert_eq!(bars_for(7_194_000), 14_388);
    }

    #[test]
    fn zkwv_header_is_exact() {
        let data = [0u8, 128, 255];
        let frame = encode_zkwv(3, &data);
        assert_eq!(&frame[0..4], b"ZKWV");
        assert_eq!(frame[4], 1);
        assert_eq!(frame[5], 0);
        assert_eq!(&frame[6..8], &[0, 0]);
        assert_eq!(&frame[8..12], &3u32.to_le_bytes());
        assert_eq!(&frame[12..], &data);
        assert_eq!(frame.len(), 12 + 3);
    }

    #[test]
    fn probe_argument_builder() {
        let args = ffprobe_args(
            "http://music/objects/x.mp3",
            Some("Authorization: Bearer t"),
        );
        assert!(args.contains(&"-show_entries".to_string()));
        assert!(args.contains(&"format=duration,format_name".to_string()));
        assert!(args.contains(&"-headers".to_string()));
        assert_eq!(args.last().unwrap(), "http://music/objects/x.mp3");
    }

    #[test]
    fn ffmpeg_argument_builder() {
        let args = ffmpeg_args("http://music/objects/x.mp3", None);
        assert!(args.windows(2).any(|w| w == ["-ac", "1"]));
        assert!(args.windows(2).any(|w| w == ["-ar", "8000"]));
        assert!(args.windows(2).any(|w| w == ["-f", "s16le"]));
        assert_eq!(args.last().unwrap(), "-");
        let with_auth = ffmpeg_args("http://x", Some("Authorization: Bearer t"));
        let header_index = with_auth.iter().position(|a| a == "-headers").unwrap();
        let input_index = with_auth.iter().position(|a| a == "-i").unwrap();
        assert!(header_index < input_index);
    }

    #[test]
    fn format_normalization() {
        assert_eq!(
            normalize_format("mp3"),
            ("mp3".to_string(), "audio/mpeg".to_string())
        );
        assert_eq!(
            normalize_format("mov,mp4,m4a,3gp,3g2,mj2"),
            ("m4a".to_string(), "audio/mp4".to_string())
        );
        assert_eq!(normalize_format("ogg").0, "ogg");
        assert_eq!(normalize_format("flac").0, "flac");
    }

    #[test]
    fn ffprobe_json_parsing() {
        let json = br#"{"format":{"duration":"7194.000000","format_name":"mp3"}}"#;
        let result = parse_ffprobe_output(json).unwrap();
        assert_eq!(result.duration_ms, 7_194_000);
        assert_eq!(result.format, "mp3");
        assert_eq!(result.mime, "audio/mpeg");
    }
}
