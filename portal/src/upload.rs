//! Streaming multipart upload (Issue #18, US-5) and the library/upload UI
//! handlers (US-9).
//!
//! Multipart fields are written to a temp file **piece by piece** (never fully
//! buffered in RAM, D6) while the SHA-256 is computed incrementally. When the
//! summed size exceeds `UPLOAD_MAX_BYTES`, the request fails with `413` and the
//! temp files are removed (their [`tempfile::TempPath`] drops).

use std::path::Path;

use askama::Template;
use axum::Extension;
use axum::extract::{Multipart, Path as AxumPath, State};
use axum::response::{Html, IntoResponse, Response};
use sha2::{Digest, Sha256};
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;

use crate::api_client::JobError;
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::PortalError;
use crate::rbac;
use crate::templates::{LibraryPage, LibraryUploadPage, UploadJobFragment, WaveformSvg};
use crate::upload_pipeline::{self, PipelineDeps};

/// A multipart file streamed to a temp file, with its hash and size.
pub struct StagedUpload {
    pub field_name: String,
    pub filename: String,
    pub content_type: String,
    pub path: TempPath,
    pub size_bytes: i64,
    pub sha256: String,
}

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

/// Streams every file part of `multipart` to a temp file under `tmp_dir`.
///
/// Only parts carrying a filename are staged (non-file form fields are
/// skipped). The summed size is capped by `max_bytes`; exceeding it is a
/// [`PortalError::PayloadTooLarge`] and all temp files are discarded.
pub async fn stage_multipart(
    multipart: &mut Multipart,
    tmp_dir: &Path,
    max_bytes: i64,
) -> Result<Vec<StagedUpload>, PortalError> {
    tokio::fs::create_dir_all(tmp_dir)
        .await
        .map_err(|e| PortalError::Internal(anyhow::anyhow!("create upload tmp dir: {e}")))?;

    let mut staged: Vec<StagedUpload> = Vec::new();
    let mut total: i64 = 0;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| PortalError::BadRequest(format!("invalid multipart body: {e}")))?
    {
        let field_name = field.name().unwrap_or("file").to_string();
        let Some(filename) = field.file_name().map(str::to_owned) else {
            continue; // not a file part
        };
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let named = tempfile::Builder::new()
            .prefix("zukka-upload-")
            .tempfile_in(tmp_dir)
            .map_err(|e| PortalError::Internal(anyhow::anyhow!("create temp file: {e}")))?;
        let path = named.into_temp_path();
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| PortalError::Internal(anyhow::anyhow!("open temp file: {e}")))?;

        let mut hasher = Sha256::new();
        let mut size: i64 = 0;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| PortalError::BadRequest(format!("multipart read error: {e}")))?
        {
            total += chunk.len() as i64;
            if total > max_bytes {
                // `staged` (and this `path`) drop here → temps are removed.
                return Err(PortalError::PayloadTooLarge);
            }
            hasher.update(&chunk);
            size += chunk.len() as i64;
            file.write_all(&chunk)
                .await
                .map_err(|e| PortalError::Internal(anyhow::anyhow!("write temp file: {e}")))?;
        }
        file.flush()
            .await
            .map_err(|e| PortalError::Internal(anyhow::anyhow!("flush temp file: {e}")))?;
        drop(file);

        let sha256 = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        staged.push(StagedUpload {
            field_name,
            filename,
            content_type,
            path,
            size_bytes: size,
            sha256,
        });
    }

    Ok(staged)
}

fn job_error(err: JobError) -> PortalError {
    if err.is_not_found() {
        PortalError::NotFound
    } else {
        PortalError::Upstream(err.to_string())
    }
}

/// `GET /library` — recent upload jobs + the upload form.
pub async fn library_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.read")?;
    let jobs = state.upload_jobs.list_jobs(50).await.unwrap_or_default();
    render(&LibraryPage {
        csrf_token: ctx.csrf_token.clone(),
        jobs,
    })
}

/// `POST /library/upload` — stage, run the pipeline, return the job fragment.
pub async fn upload_post(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    mut multipart: Multipart,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.upload")?;

    let staged = stage_multipart(
        &mut multipart,
        &state.config.upload_tmp_dir,
        state.config.upload_max_bytes,
    )
    .await?;
    if staged.is_empty() {
        return Err(PortalError::BadRequest("no file uploaded".to_string()));
    }

    let deps = PipelineDeps {
        upload_jobs: state.upload_jobs.as_ref(),
        music: state.music.as_ref(),
        waveforms: state.waveforms.as_ref(),
        ffprobe_bin: &state.config.ffprobe_bin,
        ffmpeg_bin: &state.config.ffmpeg_bin,
        tmp_dir: &state.config.upload_tmp_dir,
    };

    let mut jobs = Vec::new();
    for item in staged {
        let job_id = match upload_pipeline::run(&deps, &ctx.caster_id, item).await {
            Ok(outcome) => outcome.job_id,
            Err(err) => err.job_id,
        };
        if job_id.is_empty() {
            continue;
        }
        if let Ok(job) = state.upload_jobs.get_job(&job_id).await {
            jobs.push(job);
        }
    }

    render(&LibraryUploadPage {
        csrf_token: ctx.csrf_token.clone(),
        jobs,
    })
}

/// `GET /library/upload/{id}` — one job fragment (htmx polling target).
pub async fn upload_fragment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.read")?;
    let job = state.upload_jobs.get_job(&id).await.map_err(job_error)?;
    render(&UploadJobFragment {
        csrf_token: ctx.csrf_token.clone(),
        job,
    })
}

/// Fetches a track waveform via the write-through client; `404` when absent.
async fn fetch_track_waveform(
    state: &AppState,
    id: &str,
) -> Result<crate::api_client::Waveform, PortalError> {
    state
        .waveforms
        .get_waveform("track", id)
        .await
        .map_err(|e| PortalError::Upstream(e.to_string()))?
        .ok_or(PortalError::NotFound)
}

/// `GET /library/tracks/{id}/waveform.json` — the raw peaks as JSON (D8).
pub async fn waveform_json(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.read")?;
    let waveform = fetch_track_waveform(&state, &id).await?;
    Ok(axum::Json(waveform).into_response())
}

/// `GET /library/tracks/{id}/waveform.svg` — inline SVG rendering of the peaks.
pub async fn waveform_svg(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.read")?;
    let waveform = fetch_track_waveform(&state, &id).await?;
    let html = WaveformSvg {
        bars: waveform.bars,
        peaks: waveform.data,
    }
    .render()?;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], html).into_response())
}
