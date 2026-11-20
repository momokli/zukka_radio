//! HTML shell handlers: `GET /`, `GET /t/{slug}` and the HTML 404 fallback
//! (Issue #26, M3; plan E4).
//!
//! These share the same `public_tape` guard and `row_to_dto` mapping as the JSON
//! API — there is no second visibility rule. HTML and API keep separate error
//! domains: unknown `/api/v1/*` stays a JSON envelope, every other unknown path
//! (and an unknown/hidden slug) is the byte-identical HTML 404.

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::error::ApiResult;
use crate::shell::{self, NoScriptItem, PageData};
use crate::store::tapes::{self, Sort};

/// HTML pages are cacheable for a minute (`docs/tapes.md` §3.4).
const HTML_CACHE: &str = "public, max-age=60";
/// Errors are not cached.
const NO_STORE: &str = "no-store";

/// Content-Security-Policy for HTML routes. No `'unsafe-inline'`: the JSON
/// island is data, and every script/stylesheet is same-origin (plan T26-21).
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self' data:; media-src 'self'; font-src 'self'; connect-src 'self'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Builds an HTML response with the shared security headers.
fn html_response(status: StatusCode, cache: &str, body: String) -> Response {
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(cache) {
        headers.insert(header::CACHE_CONTROL, value);
    }
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
}

/// The last 50 public tapes as `<noscript>` entries (newest first).
async fn noscript_items(state: &AppState) -> ApiResult<Vec<NoScriptItem>> {
    let mut items = tapes::feed(
        &state.pool,
        &state.base_url,
        &[],
        None,
        None,
        Sort::Newest,
        50,
        None,
    )
    .await?;
    items.truncate(50);
    Ok(items
        .into_iter()
        .map(|item| NoScriptItem {
            slug: item.dto.slug,
            label: format!(
                "{} \u{2013} {}",
                item.dto.display_title, item.dto.window_label
            ),
        })
        .collect())
}

/// `GET /` — feed shell.
pub async fn get_index(State(state): State<AppState>) -> ApiResult<Response> {
    let noscript = noscript_items(&state).await?;
    let page = PageData {
        meta_html: shell::index_meta(&state.base_url),
        noscript_html: shell::noscript_html(&noscript),
        tape_state: "null".to_string(),
    };
    Ok(html_response(
        StatusCode::OK,
        HTML_CACHE,
        shell::render(&page),
    ))
}

/// `GET /t/{slug}` — tape shell, or the byte-identical HTML 404.
pub async fn get_tape(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> ApiResult<Response> {
    let row = match tapes::public_tape(&state.pool, &slug).await? {
        Some(row) => row,
        None => return Ok(not_found_page()),
    };

    let peaks = tapes::peaks_for(&state.pool, &row.id).await?;
    let dto = tapes::row_to_dto(&row, peaks, &state.base_url)?;
    let noscript = noscript_items(&state).await?;
    let page = PageData {
        meta_html: shell::tape_meta(&dto),
        noscript_html: shell::noscript_html(&noscript),
        tape_state: shell::json_for_html(&dto),
    };
    Ok(html_response(
        StatusCode::OK,
        HTML_CACHE,
        shell::render(&page),
    ))
}

/// Router fallback for non-API GET paths → minimal HTML 404.
///
/// Rendered without the requested path so unknown routes, unknown slugs and
/// hidden tapes all produce identical bytes (plan E4).
pub async fn html_not_found() -> Response {
    not_found_page()
}

/// The minimal HTML 404 response (shared by the handler and the fallback).
fn not_found_page() -> Response {
    html_response(StatusCode::NOT_FOUND, NO_STORE, shell::render_not_found())
}
