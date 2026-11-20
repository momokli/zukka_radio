//! Audio delivery: `GET|HEAD /media/{slug}.{ext}` (`docs/tapes.md` §3.7, M3).
//!
//! The handler proxies the byte range from the Music API object store
//! (`{MUSIC_API_URL}/objects/{storage_key}`) and streams the body without
//! buffering. `HEAD` never touches the upstream: every header is derived
//! locally from the tape row, so a range probe cannot download ~170 MB.

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::error::{ApiError, ApiResult};
use crate::etag;
use crate::media::{ByteRange, if_range_allows, parse_range};
use crate::store::tapes;
use crate::time::{download_filename, parse_storage_time};

/// Long-lived immutable caching for audio bytes (§3.7).
const AUDIO_CACHE: &str = "public, max-age=31536000, immutable";
/// Download responses bypass shared caches (`?download=1`, §3.7).
const DOWNLOAD_CACHE: &str = "private, no-transform";
/// Headers a cross-origin player must be allowed to read (§3.7).
const EXPOSE: &str = "Content-Length, Content-Range, Accept-Ranges, ETag";

/// Extensions the audio route accepts (must match `tape.format`, §3.7).
const ALLOWED_EXT: &[&str] = &["mp3", "m4a", "aac", "ogg", "opus", "flac", "wav"];

/// Shared client + credentials for the Music API object store.
#[derive(Clone)]
pub struct MediaProxy {
    /// Base URL (no trailing slash); objects live at `/objects/{key}`.
    pub url: String,
    /// Bearer token for the Music API.
    pub token: String,
    /// Shared connection pool.
    pub http: reqwest::Client,
}

impl MediaProxy {
    /// Builds a proxy with a shared `reqwest` client.
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder().build().unwrap_or_default();
        Self {
            url: url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        }
    }

    fn object_url(&self, storage_key: &str) -> String {
        format!(
            "{}/objects/{}",
            self.url,
            storage_key.trim_start_matches('/')
        )
    }
}

/// `GET /media/{file}` — streams the (optionally ranged) audio bytes.
pub async fn get_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(file): Path<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Response> {
    let ctx = match prepare(&state, &headers, &file, raw.as_deref()).await {
        Ok(ctx) => ctx,
        Err(early) => return Ok(*early),
    };
    stream_from_upstream(&state, &ctx).await
}

/// `HEAD /media/{file}` — identical headers, no body, **no upstream call**.
pub async fn head_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(file): Path<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Response> {
    let ctx = match prepare(&state, &headers, &file, raw.as_deref()).await {
        Ok(ctx) => ctx,
        Err(early) => return Ok(*early),
    };
    Ok(head_response(&ctx))
}

/// Everything both verbs need after the shared prelude (steps 1–5).
struct MediaCtx {
    slug: String,
    mime: String,
    size: u64,
    etag: String,
    range: ByteRange,
    download: bool,
    filename: Option<String>,
}

/// Shared prelude: parse path, load tape, ETag/304, `If-Range`/`Range`.
///
/// `Err` carries a fully-built response so the caller can return it directly
/// (either the byte-identical `404` or a `304`).
async fn prepare(
    state: &AppState,
    headers: &HeaderMap,
    file: &str,
    raw_query: Option<&str>,
) -> Result<MediaCtx, Box<Response>> {
    // 1. `{slug}.{ext}` — a missing dot, unknown or non-matching extension is a
    //    plain tape 404 (byte-identical to an unknown slug, R9).
    let Some((slug, ext)) = file.rsplit_once('.') else {
        return Err(Box::new(ApiError::tape_not_found().into_response()));
    };
    let ext = ext.to_ascii_lowercase();
    if !ALLOWED_EXT.contains(&ext.as_str()) {
        return Err(Box::new(ApiError::tape_not_found().into_response()));
    }

    // 2. Public guard (`visible = 1 AND status = 'ready'`).
    let row = tapes::public_tape(&state.pool, slug)
        .await
        .map_err(IntoResponse::into_response)?
        .ok_or_else(|| ApiError::tape_not_found().into_response())?;

    // The extension must match the tape's stored format (case-insensitive).
    if !row
        .format
        .as_deref()
        .is_some_and(|format| format.eq_ignore_ascii_case(&ext))
    {
        return Err(Box::new(ApiError::tape_not_found().into_response()));
    }

    // 3. ETag: prefer the object metadata, fall back to the derived hash.
    let etag = row
        .etag
        .as_deref()
        .map(quote_etag)
        .unwrap_or_else(|| etag::for_tape(&row.updated_at, &row.slug));

    let size = row.size_bytes.unwrap_or(0).max(0) as u64;
    let mime = row
        .mime
        .clone()
        .unwrap_or_else(|| mime_for(&ext).to_string());
    let download = parse_download(raw_query);
    let filename = if download {
        Some(download_filename(
            &row.caster_name,
            parse_storage_time(&row.started_at).map_err(IntoResponse::into_response)?,
            &ext,
        ))
    } else {
        None
    };

    let ctx = MediaCtx {
        slug: row.slug.clone(),
        mime,
        size,
        etag,
        range: ByteRange::Full,
        download,
        filename,
    };

    // 4. `If-None-Match` → 304.
    if etag::matches(headers, &ctx.etag) {
        return Err(Box::new(not_modified(&ctx)));
    }

    // 5. Range: a failing `If-Range` degrades to the full representation.
    let requested = header_str(headers, header::RANGE);
    let if_range = header_str(headers, header::IF_RANGE);
    let range = if if_range_allows(if_range, &ctx.etag) {
        parse_range(requested, size)
    } else {
        ByteRange::Full
    };
    if matches!(range, ByteRange::Unsatisfiable) {
        return Err(Box::new(
            ApiError::RangeNotSatisfiable { size }.into_response(),
        ));
    }

    Ok(MediaCtx { range, ..ctx })
}

/// A bare `304` with the cache/validator headers, no body.
fn not_modified(ctx: &MediaCtx) -> Response {
    let mut res = StatusCode::NOT_MODIFIED.into_response();
    let headers = res.headers_mut();
    insert_base(headers, ctx);
    apply_cache(headers, ctx);
    res
}

/// `HEAD`: build all headers locally, no upstream round-trip.
fn head_response(ctx: &MediaCtx) -> Response {
    let (status, length, content_range) = match ctx.range {
        ByteRange::Satisfiable { start, end } => (
            StatusCode::PARTIAL_CONTENT,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{}", ctx.size)),
        ),
        _ => (StatusCode::OK, ctx.size, None),
    };
    let mut res = status.into_response();
    let headers = res.headers_mut();
    insert_base(headers, ctx);
    insert_header(headers, header::CONTENT_LENGTH, &length.to_string());
    if let Some(value) = content_range {
        insert_header(headers, header::CONTENT_RANGE, &value);
    }
    apply_cache(headers, ctx);
    res
}

/// `GET`: fetch from the Music API and stream the body through.
async fn stream_from_upstream(state: &AppState, ctx: &MediaCtx) -> ApiResult<Response> {
    let storage_key = tapes::public_tape(&state.pool, &ctx.slug)
        .await?
        .and_then(|row| row.storage_key)
        .ok_or(ApiError::UpstreamUnavailable)?;

    let mut request = state
        .media
        .http
        .get(state.media.object_url(&storage_key))
        .bearer_auth(&state.media.token);
    // Only forward a `Range` when we actually want a 206.
    if let ByteRange::Satisfiable { start, end } = ctx.range {
        request = request.header(header::RANGE, format!("bytes={start}-{end}"));
    }

    let upstream = request.send().await.map_err(|err| {
        tracing::warn!("music-api object fetch failed: {err:#}");
        ApiError::UpstreamUnavailable
    })?;

    let status = upstream.status();
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // Normalize: never forward the upstream `Content-Range` unchecked.
        return Err(ApiError::RangeNotSatisfiable { size: ctx.size });
    }
    if !status.is_success() {
        tracing::warn!("music-api object fetch returned {status} for {}", ctx.slug);
        return Err(ApiError::UpstreamUnavailable);
    }

    // 206 only when the upstream honored a range we asked for; otherwise the
    // client gets the full representation.
    let ranged =
        matches!(ctx.range, ByteRange::Satisfiable { .. }) && status == StatusCode::PARTIAL_CONTENT;
    let out_status = if ranged {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let upstream_length = upstream
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let mut res = Response::new(Body::from_stream(upstream.bytes_stream()));
    *res.status_mut() = out_status;
    let headers = res.headers_mut();
    insert_base(headers, ctx);
    let (length, content_range) = match (out_status, ctx.range) {
        (StatusCode::PARTIAL_CONTENT, ByteRange::Satisfiable { start, end }) => (
            end - start + 1,
            Some(format!("bytes {start}-{end}/{}", ctx.size)),
        ),
        _ => (upstream_length.unwrap_or(ctx.size), None),
    };
    insert_header(headers, header::CONTENT_LENGTH, &length.to_string());
    if let Some(value) = content_range {
        insert_header(headers, header::CONTENT_RANGE, &value);
    }
    apply_cache(headers, ctx);
    Ok(res)
}

/// Content-Type, ETag, Accept-Ranges, expose-headers (§3.7).
fn insert_base(headers: &mut HeaderMap, ctx: &MediaCtx) {
    insert_header(headers, header::CONTENT_TYPE, &ctx.mime);
    insert_header(headers, header::ETAG, &ctx.etag);
    insert_header(headers, header::ACCEPT_RANGES, "bytes");
    insert_header(headers, header::ACCESS_CONTROL_EXPOSE_HEADERS, EXPOSE);
}

/// Cache-Control + optional `Content-Disposition` (download variant).
fn apply_cache(headers: &mut HeaderMap, ctx: &MediaCtx) {
    if ctx.download {
        insert_header(headers, header::CACHE_CONTROL, DOWNLOAD_CACHE);
        if let Some(name) = &ctx.filename {
            insert_header(
                headers,
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{name}\""),
            );
        }
    } else {
        insert_header(headers, header::CACHE_CONTROL, AUDIO_CACHE);
    }
}

fn insert_header(headers: &mut HeaderMap, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn header_str(headers: &HeaderMap, name: HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Wraps a bare object ETag in strong quotes unless already quoted/weak.
fn quote_etag(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with('"') || raw.starts_with("W/") {
        raw.to_string()
    } else {
        format!("\"{raw}\"")
    }
}

fn parse_download(raw: Option<&str>) -> bool {
    form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .any(|(key, value)| key == "download" && !value.is_empty() && value != "0")
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}
