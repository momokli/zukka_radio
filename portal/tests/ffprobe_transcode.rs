//! Unit tests for the `ffprobe` parser (US-6) and the D7 decision matrix
//! (US-7), plus a live `ffprobe`/`ffmpeg` round-trip when the binaries exist.

use std::path::Path;
use std::process::Command;

use zukka_portal::ffprobe::{self, ProbeError};
use zukka_portal::transcode::{ACCEPTED_MASTER_CODECS, needs_derivate, transcode_to_mp3};

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

// ---------------------------------------------------------------------------
// US-6 — parser
// ---------------------------------------------------------------------------

#[test]
fn parses_mp3_fixture() {
    let probe = ffprobe::parse_ffprobe_json(&fixture("mp3.json")).expect("parse");
    assert_eq!(probe.format_name.as_deref(), Some("mp3"));
    assert_eq!(probe.duration_ms, Some(180_000));
    assert_eq!(probe.bit_rate, Some(320_000));
    assert_eq!(probe.primary_audio_codec(), Some("mp3"));
    assert!(!probe.has_video());
}

#[test]
fn parses_flac_fixture() {
    let probe = ffprobe::parse_ffprobe_json(&fixture("flac.json")).expect("parse");
    assert_eq!(probe.primary_audio_codec(), Some("flac"));
    assert!(!probe.has_video());
}

#[test]
fn parses_webm_with_video_fixture() {
    let probe = ffprobe::parse_ffprobe_json(&fixture("webm_video.json")).expect("parse");
    assert!(probe.has_video());
    assert_eq!(probe.primary_audio_codec(), Some("opus"));
}

#[test]
fn parses_mp4_with_video_fixture() {
    let probe = ffprobe::parse_ffprobe_json(&fixture("mp4_video.json")).expect("parse");
    assert!(probe.has_video());
    assert_eq!(probe.primary_audio_codec(), Some("aac"));
}

#[test]
fn broken_fixture_is_parse_error() {
    let err = ffprobe::parse_ffprobe_json(&fixture("broken.json")).unwrap_err();
    assert!(matches!(err, ProbeError::Parse(_)));
}

// ---------------------------------------------------------------------------
// US-7 — D7 matrix
// ---------------------------------------------------------------------------

#[test]
fn d7_matrix() {
    // accepted codec, no video → no derivate
    for codec in ACCEPTED_MASTER_CODECS {
        assert!(!needs_derivate(Some(codec), false), "{codec}");
    }
    // any video → derivate, regardless of codec
    assert!(needs_derivate(Some("flac"), true));
    assert!(needs_derivate(Some("mp3"), true));
    // unaccepted / unknown codec → derivate
    assert!(needs_derivate(Some("wma"), false));
    assert!(needs_derivate(None, false));
}

// ---------------------------------------------------------------------------
// Live round-trip (skipped when the binaries are unavailable)
// ---------------------------------------------------------------------------

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn live_probe_and_transcode_roundtrip() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("tone.wav");
    // 0.5 s sine → PCM s16le WAV (an accepted master codec).
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.5",
        ])
        .arg(&wav)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg fixture generation");

    let probe = ffprobe::probe("ffprobe", &wav).await.expect("probe wav");
    assert!(!probe.has_video());
    assert_eq!(probe.primary_audio_codec(), Some("pcm_s16le"));
    assert!(!needs_derivate(
        probe.primary_audio_codec(),
        probe.has_video()
    ));

    // Force a derivate anyway and verify the MP3 is produced.
    let mp3 = dir.path().join("tone.mp3");
    transcode_to_mp3("ffmpeg", &wav, &mp3)
        .await
        .expect("transcode");
    assert!(mp3.exists());
    let probe_mp3 = ffprobe::probe("ffprobe", &mp3).await.expect("probe mp3");
    assert_eq!(probe_mp3.primary_audio_codec(), Some("mp3"));
}

#[tokio::test]
async fn probe_missing_binary_is_error() {
    let err = ffprobe::probe("definitely-not-ffprobe", Path::new("/dev/null")).await;
    assert!(matches!(err, Err(ProbeError::Spawn(_))));
}
