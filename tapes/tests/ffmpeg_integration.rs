//! Optional ffmpeg integration tests for the streaming RMS decoder.
//!
//! These tests skip cleanly when `ffmpeg` is not installed, so CI without the
//! binary stays green. They use `lavfi` sources instead of the Music API.

use tokio::io::AsyncReadExt;

use zukka_tapes::waveform::{CHUNK_SIZE, RmsBarEncoder};

fn have_ffmpeg() -> bool {
    std::process::Command::new(std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()))
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn encode_lavfi(input: &str, duration: &str) -> Vec<u8> {
    let bin = std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into());
    let mut child = tokio::process::Command::new(bin)
        .args([
            "-v", "error", "-f", "lavfi", "-i", input, "-t", duration, "-ac", "1", "-ar", "8000",
            "-f", "s16le", "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn ffmpeg");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut encoder = RmsBarEncoder::new();
    let mut out = Vec::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    loop {
        let read = stdout.read(&mut buffer).await.expect("read");
        if read == 0 {
            break;
        }
        out.extend(encoder.push_chunk(&buffer[..read]));
    }
    out.extend(encoder.finish());
    let status = child.wait().await.expect("wait");
    assert!(status.success(), "ffmpeg must exit 0");
    out
}

#[tokio::test]
async fn ffmpeg_silence_decodes_to_zero_bars() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }
    let bars = encode_lavfi("anullsrc=r=8000:cl=mono", "2").await;
    assert_eq!(bars.len(), 4, "2 s at 500 ms/bar");
    assert!(bars.iter().all(|byte| *byte == 0), "silence is 0 dBFS");
}

#[tokio::test]
async fn ffmpeg_full_scale_decodes_to_255_bars() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }
    let bars = encode_lavfi("aevalsrc=1:s=8000:d=2", "2").await;
    assert_eq!(bars.len(), 4, "2 s at 500 ms/bar");
    assert!(
        bars.iter().all(|byte| *byte == 255),
        "full scale is 255, got {bars:?}"
    );
}
