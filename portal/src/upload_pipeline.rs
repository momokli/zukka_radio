//! Upload pipeline orchestration + hash dedupe (Issue #18, US-8).
//!
//! Order (portal.md D6/D7/D8, §4.3):
//! 1. `create_job` (`pending`) via the [`UploadJobApi`] write-through.
//! 2. (stream → temp + SHA-256 happens in [`crate::upload`] before this runs).
//! 3. `find_by_hash` — a hit marks the job `done` with the existing track.
//! 4. `ffprobe` → D7 decision → optional MP3 derivate.
//! 5. `create_track` + `upload_asset` (master, then derivate) at the Music API.
//! 6. Waveform: compute peaks from the staged master and `put_waveform` (D8).
//! 7. `patch_job` → `done` + `track_id`; any failure → `failed` + `error`.

use std::path::Path;

use crate::api_client::{
    NewTrackRequest, NewUploadJob, NewWaveform, TrackApi, UploadJobApi, UploadJobPatch, WaveformApi,
};
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
    pub tracks: &'a dyn TrackApi,
    pub ffprobe_bin: &'a str,
    pub ffmpeg_bin: &'a str,
    pub tmp_dir: &'a Path,
}

/// Result of a successful pipeline run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineOutcome {
    pub job_id: String,
    /// The Music-API track id (external authority).
    pub track_id: String,
    /// The local `tracks.id` (US-8); `None` when an existing track was reused
    /// and the local registration hit a `409` (already present).
    pub local_track_id: Option<String>,
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

    match run_inner(deps, caster_id, &staged).await {
        Ok((track_id, local_track_id, deduped, derivate)) => {
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
                track_id,
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

/// Steps 3–6; returns `(music_id, local_id, deduped, derivate)` or a
/// human-readable reason string.
async fn run_inner(
    deps: &PipelineDeps<'_>,
    caster_id: &str,
    staged: &StagedUpload,
) -> Result<(String, Option<String>, bool, bool), String> {
    // 3. dedupe: an existing track with the same bytes short-circuits.
    match deps.music.find_by_hash(&staged.sha256).await {
        Ok(Some(existing)) => {
            // The local row already exists from the first upload: a `409`
            // (`duplicate_track`) is treated as "already present".
            let _ = deps
                .tracks
                .create(
                    Some(caster_id),
                    &NewTrackRequest {
                        music_api_id: existing.id.clone(),
                        title: existing.title.clone(),
                        storage_key: existing.id.clone(),
                        duration_ms: existing.duration_ms.unwrap_or(0),
                        artist: existing.artist.clone(),
                        album: None,
                        artwork_url: None,
                        visibility: Some("private".into()),
                        caster_id: Some(caster_id.to_string()),
                    },
                )
                .await;
            return Ok((existing.id, None, true, false));
        }
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
            title: title.clone(),
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

    // 5b. register the local library track (US-8). A `409` (duplicate
    // `music_api_id`) is tolerated as "already present".
    let local_track_id = match deps
        .tracks
        .create(
            Some(caster_id),
            &NewTrackRequest {
                music_api_id: track.id.clone(),
                title,
                storage_key: track.id.clone(),
                duration_ms: probe.duration_ms.unwrap_or(0),
                artist: track.artist.clone(),
                album: None,
                artwork_url: None,
                visibility: Some("private".into()),
                caster_id: Some(caster_id.to_string()),
            },
        )
        .await
    {
        Ok(local) => Some(local.id),
        Err(err) if err.is_conflict() => None,
        Err(err) => return Err(format!("track registration failed: {err}")),
    };

    // 6. waveform peaks (D8): computed from the master, stored write-through.
    // Keyed by the local track id so the portal detail route can resolve it.
    let subject_id = local_track_id.clone().unwrap_or_else(|| track.id.clone());
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

    Ok((track.id, local_track_id, false, needs))
}
