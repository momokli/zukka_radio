//! 2-hour streaming waveform performance smoke test (Issue #25, M3).
//!
//! Acceptance: a 2 h tape must produce its waveform in well under 60 s without
//! memory growing with duration (streaming, no full buffering). This test is
//! `#[ignore]`d so it never runs in the normal CI suite; run it explicitly:
//!
//! ```sh
//! cargo test -p zukka-tapes --test waveform_perf -- --ignored --nocapture
//! ```
//!
//! It streams 2 h of silence from ffmpeg's `lavfi` `anullsrc`, feeds the real
//! [`RmsBarEncoder`] chunk by chunk and asserts the exact bar count
//! (`7200 s / 0.5 s = 14400`). Duration and peak RSS (`VmHWM` from
//! `/proc/self/status`) are printed for the record. The test skips cleanly when
//! `ffmpeg` is not installed.

use std::time::Instant;

use tokio::io::AsyncReadExt;

use zukka_tapes::waveform::{CHUNK_SIZE, RmsBarEncoder};

const SECONDS: u64 = 7200;
const EXPECTED_BARS: usize = (SECONDS as usize) * 2; // 500 ms per bar.

fn have_ffmpeg() -> bool {
    std::process::Command::new(std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()))
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Peak resident set size in kilobytes from `/proc/self/status` (`VmHWM`).
fn peak_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .ok();
        }
    }
    None
}

#[tokio::test]
#[ignore = "2 h streaming smoke; run with --ignored (needs ffmpeg)"]
async fn two_hour_silence_streams_under_budget() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }

    let rss_before = peak_rss_kb();
    let bin = std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into());
    let duration = SECONDS.to_string();
    let started = Instant::now();

    let mut child = tokio::process::Command::new(bin)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=8000:cl=mono",
            "-t",
            &duration,
            "-ac",
            "1",
            "-ar",
            "8000",
            "-f",
            "s16le",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn ffmpeg");

    let mut stdout = child.stdout.take().expect("stdout");
    let mut encoder = RmsBarEncoder::new();
    let mut bars: Vec<u8> = Vec::with_capacity(EXPECTED_BARS);
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut streamed_bytes: u64 = 0;
    loop {
        let read = stdout.read(&mut buffer).await.expect("read");
        if read == 0 {
            break;
        }
        streamed_bytes += read as u64;
        bars.extend(encoder.push_chunk(&buffer[..read]));
    }
    bars.extend(encoder.finish());

    let status = child.wait().await.expect("wait");
    assert!(status.success(), "ffmpeg must exit 0");
    let elapsed = started.elapsed();
    let rss_after = peak_rss_kb();

    let decoded_samples = streamed_bytes / 2; // s16le mono.
    eprintln!(
        "2h waveform: bars={} decoded_samples={} streamed_bytes={} elapsed={:.2}s peak_rss_kb={:?} (before {:?})",
        bars.len(),
        decoded_samples,
        streamed_bytes,
        elapsed.as_secs_f64(),
        rss_after,
        rss_before,
    );

    assert_eq!(
        bars.len(),
        EXPECTED_BARS,
        "2 h at 500 ms/bar must yield {EXPECTED_BARS} bars"
    );
    assert!(
        bars.iter().all(|byte| *byte == 0),
        "silence must map to 0 dBFS"
    );
    assert!(
        elapsed.as_secs() < 60,
        "2 h waveform took {:.2}s, budget is < 60s",
        elapsed.as_secs_f64()
    );

    // Memory must not scale with duration: 2 h of 8 kHz mono s16le is ~110 MB,
    // a streaming decoder must stay far below that.
    if let Some(rss_kb) = rss_after {
        assert!(
            rss_kb < 100_000,
            "peak RSS {rss_kb} kB suggests the full stream was buffered"
        );
    }
}
