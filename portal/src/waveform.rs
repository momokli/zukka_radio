//! Waveform peak computation (Issue #18, D8).
//!
//! `ffmpeg` decodes the master to **mono 8 kHz signed-16-bit PCM** on stdout;
//! the portal folds it into [`WAVEFORM_BARS`] buckets of interleaved 8-bit
//! `(min, max)` pairs (portal.md D8: 800 Min/Max buckets). The peaks are stored
//! through the scheduler's write-through endpoint (`POST /internal/waveforms`,
//! P2) — the portal holds no database connection.
//!
//! `duration_ms` (already known from ffprobe) lets the decoder stream bucket by
//! bucket without buffering the whole PCM; without it the samples are buffered
//! and bucketed by count.

use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncReadExt;

/// Number of waveform buckets (portal.md D8).
pub const WAVEFORM_BARS: i64 = 800;
/// Sample rate of the decoded PCM (portal.md D8: mono 8 kHz).
pub const SAMPLE_RATE: i64 = 8_000;
/// Scheme/version stamped into every waveform row (`waveforms.scheme`).
pub const SCHEME: i64 = 1;
/// Algorithm tag stamped into every waveform row (`waveforms.algorithm`).
pub const ALGORITHM: &str = "ffmpeg-minmax-8k/1";

/// Waveform computation failure.
#[derive(Debug, thiserror::Error)]
pub enum WaveformError {
    /// `ffmpeg` could not be started (missing `FFMPEG_BIN`?).
    #[error("failed to run ffmpeg: {0}")]
    Spawn(String),
    /// `ffmpeg` exited non-zero.
    #[error("ffmpeg exited with status {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    /// Reading the decoder output failed.
    #[error("io error: {0}")]
    Io(String),
}

/// Maps an `i16` sample to 8-bit unsigned (`0x8000` offset, then `>> 8`).
///
/// `-32768 → 0`, `0 → 128`, `32767 → 255`.
fn to_u8(sample: i16) -> u8 {
    ((i32::from(sample) + 32_768) >> 8) as u8
}

/// Buckets a decoded PCM sample slice into `bars` interleaved `(min, max)` u8
/// pairs (length `2 * bars`). Pure function; an empty input yields all-zero
/// bars. Bars with no samples are `(0, 0)`.
pub fn peaks_from_pcm(samples: &[i16], bars: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bars * 2);
    let n = samples.len();
    for bar in 0..bars {
        let (mut lo, mut hi) = (i16::MAX, i16::MIN);
        if n > 0 {
            let start = bar * n / bars;
            let end = (bar + 1) * n / bars;
            for &sample in &samples[start..end] {
                lo = lo.min(sample);
                hi = hi.max(sample);
            }
            if start >= end {
                lo = 0;
                hi = 0;
            }
        } else {
            lo = 0;
            hi = 0;
        }
        out.push(to_u8(lo));
        out.push(to_u8(hi));
    }
    out
}

/// Feeds one decoded sample into either the streaming buckets (`spb` known) or
/// the fallback buffer.
fn push_sample(
    buckets: &mut Option<Vec<(i16, i16)>>,
    all: &mut Vec<i16>,
    samples_per_bar: Option<usize>,
    index: usize,
    sample: i16,
    bars: usize,
) {
    match (buckets.as_mut(), samples_per_bar) {
        (Some(buckets), Some(spb)) => {
            let bar = (index / spb).min(bars - 1);
            let (lo, hi) = &mut buckets[bar];
            *lo = (*lo).min(sample);
            *hi = (*hi).max(sample);
        }
        _ => all.push(sample),
    }
}

/// Decodes `input` with `ffmpeg` and returns `2 * bars` peak bytes.
pub async fn compute_peaks(
    ffmpeg_bin: &str,
    input: &Path,
    bars: usize,
    duration_ms: Option<i64>,
) -> Result<Vec<u8>, WaveformError> {
    if bars == 0 {
        return Ok(Vec::new());
    }

    let mut child = tokio::process::Command::new(ffmpeg_bin)
        .args(["-v", "error", "-i"])
        .arg(input)
        .args(["-ac", "1", "-ar", "8000", "-f", "s16le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| WaveformError::Spawn(e.to_string()))?;

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let samples_per_bar = duration_ms.map(|ms| {
        let samples = ms.max(0) * SAMPLE_RATE / 1000;
        (samples / bars as i64).max(1) as usize
    });
    let mut buckets: Option<Vec<(i16, i16)>> =
        samples_per_bar.map(|_| vec![(i16::MAX, i16::MIN); bars]);
    let mut all: Vec<i16> = Vec::new();
    let mut carry: Option<u8> = None;
    let mut index: usize = 0;
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let n = stdout
            .read(&mut buf)
            .await
            .map_err(|e| WaveformError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        let mut start = 0;
        if let Some(high) = carry.take() {
            let sample = i16::from_le_bytes([high, buf[0]]);
            push_sample(&mut buckets, &mut all, samples_per_bar, index, sample, bars);
            index += 1;
            start = 1;
        }
        let mut i = start;
        while i + 1 < n {
            let sample = i16::from_le_bytes([buf[i], buf[i + 1]]);
            push_sample(&mut buckets, &mut all, samples_per_bar, index, sample, bars);
            index += 1;
            i += 2;
        }
        if i < n {
            carry = Some(buf[i]);
        }
    }
    drop(stdout);

    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr).await;
    }
    let status = child
        .wait()
        .await
        .map_err(|e| WaveformError::Io(e.to_string()))?;
    if !status.success() {
        return Err(WaveformError::Exit {
            status: status.code().unwrap_or(-1),
            stderr: stderr.trim().to_string(),
        });
    }

    match buckets {
        Some(buckets) => {
            let mut out = Vec::with_capacity(bars * 2);
            for (lo, hi) in buckets {
                let (lo, hi) = if lo == i16::MAX && hi == i16::MIN {
                    (0, 0)
                } else {
                    (lo, hi)
                };
                out.push(to_u8(lo));
                out.push(to_u8(hi));
            }
            Ok(out)
        }
        None => Ok(peaks_from_pcm(&all, bars)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_u8_maps_full_range() {
        assert_eq!(to_u8(-32_768), 0);
        assert_eq!(to_u8(0), 128);
        assert_eq!(to_u8(32_767), 255);
    }

    #[test]
    fn empty_pcm_yields_zero_bars() {
        let peaks = peaks_from_pcm(&[], 4);
        assert_eq!(peaks.len(), 8);
        assert!(
            peaks.iter().all(|b| *b == 128),
            "silence is 0x80: {peaks:?}"
        );
    }

    #[test]
    fn peaks_track_min_and_max_per_bar() {
        // 4 samples, 2 bars: bar0 = [-100, 100], bar1 = [-1000, 1000].
        let samples = [-100i16, 100, -1000, 1000];
        let peaks = peaks_from_pcm(&samples, 2);
        assert_eq!(peaks.len(), 4);
        assert_eq!(peaks[0], to_u8(-100));
        assert_eq!(peaks[1], to_u8(100));
        assert_eq!(peaks[2], to_u8(-1000));
        assert_eq!(peaks[3], to_u8(1000));
    }

    #[test]
    fn more_bars_than_samples_pads_with_silence() {
        let peaks = peaks_from_pcm(&[32_767, -32_768], 4);
        assert_eq!(peaks.len(), 8);
        // Both samples are captured somewhere in the output.
        assert!(peaks.contains(&to_u8(32_767)), "{peaks:?}");
        assert!(peaks.contains(&to_u8(-32_768)), "{peaks:?}");
        // Bars with no samples are silence (0x80).
        let silent = peaks.iter().filter(|b| **b == 128).count();
        assert!(silent >= 4, "expected mostly silence: {peaks:?}");
    }

    #[tokio::test]
    async fn compute_peaks_from_generated_sine() {
        let have = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: ffmpeg missing");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("tone.wav");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
            ])
            .arg(&wav)
            .status()
            .expect("run ffmpeg");
        assert!(status.success());

        let peaks = compute_peaks("ffmpeg", &wav, WAVEFORM_BARS as usize, Some(1000))
            .await
            .expect("peaks");
        assert_eq!(peaks.len(), WAVEFORM_BARS as usize * 2);
        // A 440 Hz sine must produce a non-trivial waveform.
        assert!(
            peaks.iter().any(|b| *b != 128),
            "expected a signal: {peaks:?}"
        );

        // The fallback path (no duration) yields the same shape.
        let fallback = compute_peaks("ffmpeg", &wav, WAVEFORM_BARS as usize, None)
            .await
            .expect("peaks");
        assert_eq!(fallback.len(), WAVEFORM_BARS as usize * 2);
    }
}
