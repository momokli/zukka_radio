//! Tape handlers: `GET /api/v1/tapes` (feed) and `GET /api/v1/tapes/{slug}`.

use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::api::AppState;
use crate::cursor::{self, Cursor};
use crate::error::{ApiError, ApiResult};
use crate::etag;
use crate::model::{TapeDto, TapeFeedDto};
use crate::store::tapes::{self, Sort};
use crate::time::{berlin_day_end_utc, berlin_day_start_utc, fmt_utc_z, parse_date};

const DETAIL_CACHE: &str = "public, max-age=300";
const FEED_CACHE: &str = "public, max-age=60";

/// `GET /api/v1/tapes/{slug}` — one public tape or a byte-identical `404`.
pub async fn get_tape(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> ApiResult<Response> {
    let row = tapes::public_tape(&state.pool, &slug)
        .await?
        .ok_or_else(ApiError::tape_not_found)?;

    let etag = etag::for_tape(&row.updated_at, &row.slug);
    if etag::matches(&headers, &etag) {
        return Ok(not_modified(&etag, DETAIL_CACHE));
    }

    let peaks = tapes::peaks_for(&state.pool, &row.id).await?;
    let dto = tapes::row_to_dto(&row, peaks, &state.base_url)?;
    Ok(json_with(&dto, StatusCode::OK, &etag, DETAIL_CACHE))
}

/// `GET /api/v1/tapes` — filtered, keyset-paginated feed.
pub async fn get_tapes(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult<Response> {
    let params = parse_feed(raw.as_deref())?;

    let mut items = tapes::feed(
        &state.pool,
        &state.base_url,
        &params.casters,
        params.from_utc,
        params.to_utc,
        params.sort,
        params.limit,
        params.cursor.as_ref(),
    )
    .await?;

    let mut next_cursor = None;
    if items.len() as i64 > params.limit {
        items.truncate(params.limit as usize);
        if let Some(last) = items.last() {
            next_cursor = Some(cursor::encode(&Cursor {
                t: last.started_at_ms,
                s: last.dto.slug.clone(),
            }));
        }
    }

    let parts: Vec<String> = items
        .iter()
        .map(|item| format!("{}|{}", item.dto.slug, item.dto.updated_at))
        .collect();
    let etag = etag::for_feed(params.sort_str, next_cursor.as_deref(), &parts);
    if etag::matches(&headers, &etag) {
        return Ok(not_modified(&etag, FEED_CACHE));
    }

    let tapes: Vec<TapeDto> = items.into_iter().map(|item| item.dto).collect();
    let body = TapeFeedDto {
        count: tapes.len(),
        tapes,
        next_cursor,
        server_time: fmt_utc_z(Utc::now()),
    };
    Ok(json_with(&body, StatusCode::OK, &etag, FEED_CACHE))
}

/// Parsed and validated feed query parameters.
struct FeedParams {
    casters: Vec<String>,
    from_utc: Option<DateTime<Utc>>,
    to_utc: Option<DateTime<Utc>>,
    sort: Sort,
    sort_str: &'static str,
    limit: i64,
    cursor: Option<Cursor>,
}

fn parse_feed(raw: Option<&str>) -> ApiResult<FeedParams> {
    let mut casters: Vec<String> = Vec::new();
    let mut from = None;
    let mut to = None;
    let mut sort_raw: Option<String> = None;
    let mut limit_raw: Option<String> = None;
    let mut cursor_raw: Option<String> = None;

    for (key, value) in form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        match key.as_ref() {
            "caster" => {
                for part in value.split(',') {
                    let part = part.trim();
                    if !part.is_empty() {
                        casters.push(part.to_owned());
                    }
                }
            }
            "from" => from = Some(parse_date("from", &value)?),
            "to" => to = Some(parse_date("to", &value)?),
            "sort" => sort_raw = Some(value.into_owned()),
            "limit" => limit_raw = Some(value.into_owned()),
            "cursor" => cursor_raw = Some(value.into_owned()),
            _ => {}
        }
    }

    let sort = match sort_raw.as_deref() {
        None => Sort::Newest,
        Some(value) => Sort::parse(value)?,
    };
    let sort_str = if sort == Sort::Oldest {
        "oldest"
    } else {
        "newest"
    };

    let limit = match limit_raw {
        None => 50,
        Some(raw) => raw
            .trim()
            .parse::<i64>()
            .map_err(|_| ApiError::bad_request("invalid limit: expected an integer 1..=200"))?,
    };
    if !(1..=200).contains(&limit) {
        return Err(ApiError::bad_request(
            "invalid limit: expected an integer 1..=200",
        ));
    }

    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return Err(ApiError::bad_request(
                "invalid range: from must not be after to",
            ));
        }
    }

    let cursor = match cursor_raw {
        Some(raw) => Some(cursor::decode(&raw)?),
        None => None,
    };

    Ok(FeedParams {
        casters,
        from_utc: from.map(berlin_day_start_utc),
        to_utc: to.map(berlin_day_end_utc),
        sort,
        sort_str,
        limit,
        cursor,
    })
}

fn not_modified(etag: &str, cache: &str) -> Response {
    let mut res = StatusCode::NOT_MODIFIED.into_response();
    insert_cache_headers(res.headers_mut(), etag, cache);
    res
}

fn json_with<T: Serialize>(value: &T, status: StatusCode, etag: &str, cache: &str) -> Response {
    let mut res = (status, Json(value)).into_response();
    insert_cache_headers(res.headers_mut(), etag, cache);
    res
}

fn insert_cache_headers(headers: &mut HeaderMap, etag: &str, cache: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    if let Ok(value) = HeaderValue::from_str(cache) {
        headers.insert(header::CACHE_CONTROL, value);
    }
}
