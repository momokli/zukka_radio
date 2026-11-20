//! `GET|HEAD /api/v1/tapes/{slug}/waveform` — ZKWV peaks (Issue #25, M3).
//!
//! The public guard runs first: an unknown, hidden, pending, processing,
//! failed or deleted tape yields the byte-identical `404` envelope. A ready
//! tape without a waveform row is treated the same way (no existence leak).

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::error::{ApiError, ApiResult};
use crate::etag;
use crate::store::tapes;
use crate::waveform::encode_zkwv;

const WAVEFORM_CACHE: &str = "public, max-age=31536000, immutable";
const CONTENT_TYPE: &str = "application/octet-stream";

/// `GET /api/v1/tapes/{slug}/waveform` — binary ZKWV frame.
pub async fn get_waveform(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> ApiResult<Response> {
    let row = tapes::public_tape(&state.pool, &slug)
        .await?
        .ok_or_else(ApiError::tape_not_found)?;

    let waveform = tapes::waveform_for(&state.pool, &row.id)
        .await?
        .ok_or_else(ApiError::tape_not_found)?;

    if waveform.bars < 0 || waveform.bars as usize != waveform.data.len() {
        tracing::error!(
            slug = %row.slug,
            bars = waveform.bars,
            bytes = waveform.data.len(),
            "inconsistent waveform row"
        );
        return Err(ApiError::tape_not_found());
    }

    let etag = etag::for_waveform(&row.slug, waveform.bars);
    if etag::matches(&headers, &etag) {
        return Ok(not_modified(&etag));
    }

    let body_len = 12usize + waveform.data.len();
    let body = encode_zkwv(waveform.bars as u32, &waveform.data);
    debug_assert_eq!(body.len(), body_len);
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    insert_headers(response.headers_mut(), &etag, Some(body_len));
    Ok(response)
}

fn not_modified(etag: &str) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    insert_headers(response.headers_mut(), etag, None);
    response
}

fn insert_headers(headers: &mut HeaderMap, etag: &str, content_length: Option<usize>) {
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE));
    if let Some(length) = content_length {
        if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
            headers.insert(header::CONTENT_LENGTH, value);
        }
    }
    if let Ok(value) = etag.parse() {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(WAVEFORM_CACHE),
    );
}
