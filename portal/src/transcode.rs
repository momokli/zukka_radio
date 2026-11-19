//! Transcode decision + stream derivate (Issue #18, US-7; portal.md D7).
//!
//! The master (original) is always kept. A stream derivate — MP3 320 CBR,
//! 44.1 kHz, stereo — is produced only when the master codec is not in the
//! accepted set **or** the container carries a video stream.

use std::path::Path;
use std::process::Stdio;

/// Master codecs Liquidsoap can decode directly (D7); everything else needs a
/// derivate.
pub const ACCEPTED_MASTER_CODECS: [&str; 9] = [
    "mp3",
    "flac",
    "vorbis",
    "opus",
    "aac",
    "alac",
    "pcm_s16le",
    "pcm_s24le",
    "aiff",
];

/// `true` when a stream derivate must be produced (D7).
///
/// * a video stream is present → `true` (e.g. `webm`/`mp4` from link
///   resolution),
/// * an unknown/missing codec → `true` (fail-safe),
/// * otherwise `true` only for a codec outside [`ACCEPTED_MASTER_CODECS`].
pub fn needs_derivate(codec: Option<&str>, has_video: bool) -> bool {
    if has_video {
        return true;
    }
    match codec {
        Some(codec) => !ACCEPTED_MASTER_CODECS.contains(&codec),
        None => true,
    }
}

/// Transcode failure.
#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    /// `ffmpeg` could not be started (missing `FFMPEG_BIN`?).
    #[error("failed to run ffmpeg: {0}")]
    Spawn(String),
    /// `ffmpeg` exited non-zero.
    #[error("ffmpeg exited with status {status}: {stderr}")]
    Exit { status: i32, stderr: String },
}

/// Encodes `input` to a 320 kbps CBR 44.1 kHz stereo MP3 at `output` (D7).
///
/// `output` may already exist (a fresh temp file); `-y` overwrites it. The
/// video stream is dropped (`-vn`).
pub async fn transcode_to_mp3(
    ffmpeg_bin: &str,
    input: &Path,
    output: &Path,
) -> Result<(), TranscodeError> {
    let status = tokio::process::Command::new(ffmpeg_bin)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args([
            "-vn",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "320k",
            "-ar",
            "44100",
            "-ac",
            "2",
        ])
        .arg(output)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| TranscodeError::Spawn(e.to_string()))?;

    if !status.status.success() {
        return Err(TranscodeError::Exit {
            status: status.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&status.stderr).trim().to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_codecs_without_video_need_no_derivate() {
        for codec in ACCEPTED_MASTER_CODECS {
            assert!(
                !needs_derivate(Some(codec), false),
                "{codec} should not need a derivate"
            );
        }
    }

    #[test]
    fn unaccepted_codec_needs_derivate() {
        for codec in ["wma", "ac3", "dts", "pcm_f32le"] {
            assert!(
                needs_derivate(Some(codec), false),
                "{codec} should need a derivate"
            );
        }
    }

    #[test]
    fn video_always_needs_derivate() {
        assert!(needs_derivate(Some("flac"), true));
        assert!(needs_derivate(Some("aac"), true));
    }

    #[test]
    fn missing_codec_needs_derivate() {
        assert!(needs_derivate(None, false));
    }
}
