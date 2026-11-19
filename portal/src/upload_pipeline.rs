//! Upload pipeline orchestration + hash dedupe (Issue #18, US-8).
//!
//! Order (portal.md D6/D7/D8, §4.3):
//! 1. `create_job` (`pending`) via the [`UploadJobApi`] write-through.
//! 2. (stream → temp + SHA-256 happens in [`crate::upload`] before this runs).
//! 3. `find_by_hash` — a hit marks the job `done` with the existing track.
//! 4. `ffprobe` → D7 decision → optional MP3 derivate.
//! 5. `create_track` + `upload_asset` (master, then derivate) at the Music API.
//! 6. `patch_job` → `done` + `track_id`; any failure → `failed` + `error`.

use std::path::Path;

use crate::api_client::{NewUploadJob, NewWaveform, UploadJobApi, UploadJobPatch, WaveformApi};
use crate::error::PortalError;
use crate::ffprobe;
use crate::music_api::{MusicApi, NewTrack};
use crate::transcode;
use crate::upload::StagedUpload;
use crate::waveform;

/// Collaborators the pipeline needs (all swappable for tests).
pub struct PipelineDeps<'a> {
    pub upload_jobs: &'a dyn UploadJobApi,
    pub music: &'a dyn MusicApi,
    pub waveforms: &'a dyn WaveformApi,
    pub ffprobe_bin: &'a str,
    pub ffmpeg_bin: &'a str,
    pub tmp_dir: &'a Path,
}

/// Result of a successful pipeline run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineOutcome {
    pub job_id: String,
    pub track_id: String,
    /// `true` when dedupe matched an existing track (no new track created).
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

    match run_inner(deps, &staged).await {
        Ok((track_id, deduped, derivate)) => {
            let _ = deps
                .upload_jobs
                .patch_job(
                    &job_id,
                    &UploadJobPatch {
                        state: "done".into(),
                        sha256: None,
                        track_id: Some(track_id.clone()),
                        error: None,
                    },
                )
                .await;
            Ok(PipelineOutcome {
                job_id,
                track_id,
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

/// Steps 3–5; returns `(track_id, deduped, derivate)` or a human-readable
/// reason string.
async fn run_inner(
    deps: &PipelineDeps<'_>,
    staged: &StagedUpload,
) -> Result<(String, bool, bool), String> {
    // 3. dedupe: an existing track with the same bytes short-circuits.
    match deps.music.find_by_hash(&staged.sha256).await {
        Ok(Some(existing)) => return Ok((existing.id, true, false)),
        Ok(None) => {}
        Err(err) => return Err(format!("hash lookup failed: {err}")),
    }

    // 4. probe + D7 decision + optional derivate.
    let probe = ffprobe::probe(deps.ffprobe_bin, &staged.path)
        .await
        .map_err(|e| format!("ffprobe failed: {e}"))?;
    let needs = transcode::needs_derivate(probe.primary_audio_codec(), probe.has_video());

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

    // 5. create track, upload master (and derivate).
    let title = staged
        .filename
        .rsplit('/')
        .next()
        .unwrap_or(&staged.filename)
        .to_string();
    let track = deps
        .music
        .create_track(&NewTrack {
            title,
            artist: None,
            sha256: Some(staged.sha256.clone()),
        })
        .await
        .map_err(|e| format!("create_track failed: {e}"))?;

    deps.music
        .upload_asset(&track.id, &staged.path, &staged.content_type)
        .await
        .map_err(|e| format!("master upload failed: {e}"))?;
    if let Some(path) = &derivate_path {
        deps.music
            .upload_asset(&track.id, path, "audio/mpeg")
            .await
            .map_err(|e| format!("derivate upload failed: {e}"))?;
    }

    // 6. waveform peaks (D8): computed from the master, stored write-through.
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
            subject_id: track.id.clone(),
            scheme: waveform::SCHEME,
            algorithm: waveform::ALGORITHM.into(),
            ms_per_bar,
            bars: waveform::WAVEFORM_BARS,
            data: peaks,
        })
        .await
        .map_err(|e| format!("waveform write failed: {e}"))?;

    Ok((track.id, false, needs))
}
