//! Upload pipeline orchestration + object dedupe (Issue #18, US-8; #134 US-3).
//!
//! Order (portal.md D6/D7/D8, §4.3):
//! 1. `create_job` (`pending`) via the [`UploadJobApi`] write-through.
//! 2. (stream → temp + SHA-256 happens in [`crate::upload`] before this runs).
//! 3. `has_object` (`HEAD /objects/{sha256}`) — a hit marks the job `done`
//!    with the existing object and registers the local row.
//! 4. `ffprobe` → D7 decision → optional MP3 derivate. An upload with an ISRC
//!    always needs an MP3 derivate when the master is not MP3, because the
//!    playback route `GET /isrc/{isrc}/320` resolves the newest `kind=mp3`
//!    object (Issue #134).
//! 5. `put_object` (`PUT /objects/{sha256}`, master then derivate); on a new
//!    store `enqueue_work("analyze", sha256)` (best-effort).
//! 6. Waveform: compute peaks from the staged master and `put_waveform` (D8).
//! 7. `patch_job` → `done` + local track id; any failure → `failed` + `error`.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::api_client::{
    NewTrackRequest, NewUploadJob, NewWaveform, TrackApi, UploadJobApi, UploadJobPatch, WaveformApi,
};
use crate::error::PortalError;
use crate::ffprobe;
use crate::music_api::{MusicApi, ObjectUpload, StoreOutcome};
use crate::transcode;
use crate::upload::StagedUpload;
use crate::waveform;

/// Collaborators the pipeline needs (all swappable for tests).
pub struct PipelineDeps<'a> {
    pub upload_jobs: &'a dyn UploadJobApi,
    pub music: &'a dyn MusicApi,
    pub waveforms: &'a dyn WaveformApi,
    pub tracks: &'a dyn TrackApi,
    pub ffprobe_bin: &'a str,
    pub ffmpeg_bin: &'a str,
    pub tmp_dir: &'a Path,
}

/// Result of a successful pipeline run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineOutcome {
    pub job_id: String,
    /// The Music-API object hash (sha256) of the uploaded master.
    pub music_object_hash: String,
    /// The local `tracks.id` (US-8); `None` when an existing track was reused
    /// and the local registration hit a `409` (already present).
    pub local_track_id: Option<String>,
    /// `true` when dedupe matched an existing object (no new upload).
    pub deduped: bool,
    /// `true` when a stream derivate was produced (D7).
    pub derivate: bool,
}

/// A failed pipeline run. The job has already been marked `failed`.
#[derive(Debug, thiserror::Error)]
#[error("upload job {job_id} failed: {message}")]
pub struct PipelineError {
    pub job_id: String,
    pub message: String,
}

/// Maps any pipeline error into a [`PortalError`] for the handler.
impl From<PipelineError> for PortalError {
    fn from(err: PipelineError) -> Self {
        PortalError::Upstream(err.message)
    }
}

/// Runs the full pipeline for one staged file. The job is always terminal
/// afterwards (`done` or `failed`).
pub async fn run(
    deps: &PipelineDeps<'_>,
    caster_id: &str,
    staged: StagedUpload,
) -> Result<PipelineOutcome, PipelineError> {
    // 1. create job (pending)
    let job_id = match deps
        .upload_jobs
        .create_job(&NewUploadJob {
            caster_id: caster_id.to_string(),
            filename: staged.filename.clone(),
            size_bytes: Some(staged.size_bytes),
            sha256: Some(staged.sha256.clone()),
        })
        .await
    {
        Ok(id) => id,
        // No job exists yet — nothing to mark failed.
        Err(err) => {
            return Err(PipelineError {
                job_id: String::new(),
                message: format!("could not create upload job: {err}"),
            });
        }
    };

    // 2. running (records the hash; increments attempts)
    let _ = deps
        .upload_jobs
        .patch_job(
            &job_id,
            &UploadJobPatch {
                state: "running".into(),
                sha256: Some(staged.sha256.clone()),
                track_id: None,
                error: None,
            },
        )
        .await;

    match run_inner(deps, caster_id, &staged).await {
        Ok((music_object_hash, local_track_id, deduped, derivate)) => {
            let _ = deps
                .upload_jobs
                .patch_job(
                    &job_id,
                    &UploadJobPatch {
                        state: "done".into(),
                        sha256: None,
                        track_id: local_track_id.clone(),
                        error: None,
                    },
                )
                .await;
            Ok(PipelineOutcome {
                job_id,
                music_object_hash,
                local_track_id,
                deduped,
                derivate,
            })
        }
        Err(message) => {
            let _ = deps
                .upload_jobs
                .patch_job(
                    &job_id,
                    &UploadJobPatch {
                        state: "failed".into(),
                        sha256: None,
                        track_id: None,
                        error: Some(message.clone()),
                    },
                )
                .await;
            Err(PipelineError { job_id, message })
        }
    }
}

/// Lowercase-hex sha256 of a file, read in chunks (never fully buffered).
async fn sha256_file(path: &Path) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open derivate: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| format!("read derivate: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The basename of an upload filename (no directory parts).
fn basename(filename: &str) -> String {
    filename.rsplit('/').next().unwrap_or(filename).to_string()
}

/// Makes a value safe for an HTTP header (`X-Name`/`X-Original-Path`).
///
/// The Music API reads these headers with `HeaderValue::to_str()`, which only
/// accepts visible ASCII; anything else makes it drop the header, so
/// `classify_kind` never sees the `.mp3` extension and `GET /isrc/{isrc}/320`
/// 404s. Replace every non-ASCII/control character while keeping the
/// (ASCII) filename extension intact.
fn ascii_header_value(value: &str) -> String {
    let mut out: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii() && !ch.is_ascii_control() {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.trim_matches('_').is_empty() {
        out = "upload".to_string();
    }
    out
}

/// `true` when an `ffprobe` codec name denotes MP3 (the codec `320` resolves to).
fn codec_is_mp3(codec: Option<&str>) -> bool {
    match codec {
        Some(codec) => {
            let codec = codec.trim().to_ascii_lowercase();
            codec == "mp3" || codec.starts_with("mp3")
        }
        None => false,
    }
}

/// Registers the local library track; a `409` (duplicate `music_api_id`) is
/// tolerated as "already present" and yields `None`.
async fn register_local(
    deps: &PipelineDeps<'_>,
    caster_id: &str,
    sha256: &str,
    title: String,
    duration_ms: i64,
    artist: Option<String>,
    isrc: Option<String>,
) -> Result<Option<String>, String> {
    match deps
        .tracks
        .create(
            Some(caster_id),
            &NewTrackRequest {
                music_api_id: sha256.to_string(),
                isrc,
                title,
                storage_key: sha256.to_string(),
                duration_ms,
                artist,
                album: None,
                artwork_url: None,
                visibility: Some("private".into()),
                caster_id: Some(caster_id.to_string()),
            },
        )
        .await
    {
        Ok(local) => Ok(Some(local.id)),
        Err(err) if err.is_conflict() => Ok(None),
        Err(err) => Err(format!("track registration failed: {err}")),
    }
}

/// Steps 3–6; returns `(music_object_hash, local_id, deduped, derivate)` or a
/// human-readable reason string.
async fn run_inner(
    deps: &PipelineDeps<'_>,
    caster_id: &str,
    staged: &StagedUpload,
) -> Result<(String, Option<String>, bool, bool), String> {
    let title = basename(&staged.filename);

    // 3. dedupe: an existing object with the same bytes short-circuits.
    match deps.music.has_object(&staged.sha256).await {
        Ok(true) => {
            let _ = register_local(
                deps,
                caster_id,
                &staged.sha256,
                title,
                0,
                None,
                staged.isrc.clone(),
            )
            .await?;
            return Ok((staged.sha256.clone(), None, true, false));
        }
        Ok(false) => {}
        Err(err) => return Err(format!("object lookup failed: {err}")),
    }

    // 4. probe + D7 decision + optional derivate.
    let probe = ffprobe::probe(deps.ffprobe_bin, &staged.path)
        .await
        .map_err(|e| format!("ffprobe failed: {e}"))?;
    let codec = probe.primary_audio_codec();
    // D7 plus the ISRC contract: `GET /isrc/{isrc}/320` resolves the newest
    // object whose `kind` is `mp3` (from the `.mp3` name extension). A
    // lossless or WAV master is an accepted codec, so D7 alone would skip the
    // derivate and the ISRC playback route would 404. With an ISRC set, always
    // produce an MP3 320 derivate unless the master itself is MP3.
    let needs = transcode::needs_derivate(codec, probe.has_video())
        || (staged.isrc.is_some() && !codec_is_mp3(codec));

    let derivate_path = if needs {
        let out = tempfile::Builder::new()
            .prefix("zukka-derivate-")
            .tempfile_in(deps.tmp_dir)
            .map_err(|e| format!("could not create derivate temp file: {e}"))?;
        let out_path = out.into_temp_path();
        transcode::transcode_to_mp3(deps.ffmpeg_bin, &staged.path, &out_path)
            .await
            .map_err(|e| format!("transcode failed: {e}"))?;
        Some(out_path)
    } else {
        None
    };

    // 5. upload the master object (ASCII-safe header values, see above).
    let title_header = ascii_header_value(&title);
    let original_path_header = ascii_header_value(&staged.filename);
    let master = ObjectUpload {
        sha256: &staged.sha256,
        path: &staged.path,
        content_type: &staged.content_type,
        isrc: staged.isrc.as_deref(),
        name: Some(&title_header),
        original_path: Some(&original_path_header),
    };
    let master_outcome = deps
        .music
        .put_object(&master)
        .await
        .map_err(|e| format!("master upload failed: {e}"))?;
    if master_outcome == StoreOutcome::Stored {
        enqueue_analyze(deps, &staged.sha256).await;
    }

    // 5a. upload the derivate as its own object (same ISRC + `.mp3` so
    //     `GET /isrc/{isrc}/320` resolves it via `classify_kind`).
    if let Some(path) = &derivate_path {
        let derivate_hash = sha256_file(path).await?;
        let stem = Path::new(&staged.filename)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| staged.filename.clone());
        let derivate_name = ascii_header_value(&format!("{stem}.mp3"));
        let derivate = ObjectUpload {
            sha256: &derivate_hash,
            path,
            content_type: "audio/mpeg",
            isrc: staged.isrc.as_deref(),
            name: Some(&derivate_name),
            original_path: Some(&derivate_name),
        };
        let derivate_outcome = deps
            .music
            .put_object(&derivate)
            .await
            .map_err(|e| format!("derivate upload failed: {e}"))?;
        if derivate_outcome == StoreOutcome::Stored {
            enqueue_analyze(deps, &derivate_hash).await;
        }
    }

    // 5b. register the local library track (US-8).
    let local_track_id = register_local(
        deps,
        caster_id,
        &staged.sha256,
        title,
        probe.duration_ms.unwrap_or(0),
        None,
        staged.isrc.clone(),
    )
    .await?;

    // 6. waveform peaks (D8): computed from the master, stored write-through.
    // Keyed by the local track id so the portal detail route can resolve it.
    let subject_id = local_track_id
        .clone()
        .unwrap_or_else(|| staged.sha256.clone());
    let peaks = waveform::compute_peaks(
        deps.ffmpeg_bin,
        &staged.path,
        waveform::WAVEFORM_BARS as usize,
        probe.duration_ms,
    )
    .await
    .map_err(|e| format!("waveform computation failed: {e}"))?;
    let ms_per_bar = probe
        .duration_ms
        .map(|ms| (ms / waveform::WAVEFORM_BARS).max(0))
        .unwrap_or(0);
    deps.waveforms
        .put_waveform(&NewWaveform {
            subject_kind: "track".into(),
            subject_id,
            scheme: waveform::SCHEME,
            algorithm: waveform::ALGORITHM.into(),
            ms_per_bar,
            bars: waveform::WAVEFORM_BARS,
            data: peaks,
        })
        .await
        .map_err(|e| format!("waveform write failed: {e}"))?;

    Ok((staged.sha256.clone(), local_track_id, false, needs))
}

/// `POST /work {"kind":"analyze"}` — best-effort (E4): a failure is logged but
/// never fails the job.
async fn enqueue_analyze(deps: &PipelineDeps<'_>, sha256: &str) {
    if let Err(err) = deps.music.enqueue_work("analyze", sha256).await {
        tracing::warn!(sha256, %err, "analyze work enqueue failed (ignored)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_header_value_keeps_extensions_and_drops_non_ascii() {
        assert_eq!(ascii_header_value("tone.mp3"), "tone.mp3");
        assert_eq!(ascii_header_value("Lied – ä.mp3"), "Lied _ _.mp3");
        assert!(ascii_header_value("日本語.flac").is_ascii());
        assert!(ascii_header_value("日本語.flac").ends_with(".flac"));
        assert_eq!(ascii_header_value("日本語"), "upload");
        assert_eq!(ascii_header_value("\u{1}"), "upload");
    }

    #[test]
    fn codec_is_mp3_normalizes_case_and_whitespace() {
        assert!(codec_is_mp3(Some("mp3")));
        assert!(codec_is_mp3(Some(" MP3 ")));
        assert!(!codec_is_mp3(Some("flac")));
        assert!(!codec_is_mp3(Some("pcm_s16le")));
        assert!(!codec_is_mp3(None));
    }
}
