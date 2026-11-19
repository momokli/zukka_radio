//! Waveform peak store (Issue #18, D8/C05).
//!
//! One `waveforms` table serves tracks **and** tapes, keyed by
//! `(subject_kind, subject_id)` (C05). The scheduler is the single writer (P2);
//! the portal and the tapes-worker reach these functions only through
//! `POST`/`GET /internal/waveforms`. `data` holds `2 * bars` bytes of
//! interleaved 8-bit `(min, max)` pairs.

use sqlx::SqlitePool;

use crate::dto::{UpsertWaveformRequest, WaveformDto};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;

/// Legal `subject_kind` values (`waveforms.subject_kind` CHECK).
pub const SUBJECT_KINDS: [&str; 2] = ["track", "tape"];

#[derive(Debug, sqlx::FromRow)]
struct WaveformRow {
    subject_kind: String,
    subject_id: String,
    scheme: i64,
    algorithm: String,
    ms_per_bar: i64,
    bars: i64,
    data: Vec<u8>,
    created_at: String,
}

impl WaveformRow {
    fn into_dto(self) -> WaveformDto {
        WaveformDto {
            subject_kind: self.subject_kind,
            subject_id: self.subject_id,
            scheme: self.scheme,
            algorithm: self.algorithm,
            ms_per_bar: self.ms_per_bar,
            bars: self.bars,
            data: self.data,
            created_at: self.created_at,
        }
    }
}

const SELECT: &str = "SELECT subject_kind, subject_id, scheme, algorithm, ms_per_bar, bars, \
     data, created_at FROM waveforms";

/// Validates an upsert request; field-level problems become `422`.
pub fn validate(req: &UpsertWaveformRequest) -> Result<(), ApiError> {
    if !SUBJECT_KINDS.contains(&req.subject_kind.as_str()) {
        return Err(ApiError::validation(
            "subject_kind",
            format!(
                "unknown subject_kind '{}', expected 'track' or 'tape'",
                req.subject_kind
            ),
        ));
    }
    if req.subject_id.trim().is_empty() {
        return Err(ApiError::validation(
            "subject_id",
            "subject_id must not be empty",
        ));
    }
    if req.scheme < 1 {
        return Err(ApiError::validation("scheme", "scheme must be >= 1"));
    }
    if req.algorithm.trim().is_empty() {
        return Err(ApiError::validation(
            "algorithm",
            "algorithm must not be empty",
        ));
    }
    if req.ms_per_bar < 0 {
        return Err(ApiError::validation(
            "ms_per_bar",
            "ms_per_bar must be >= 0",
        ));
    }
    if req.bars < 0 {
        return Err(ApiError::validation("bars", "bars must be >= 0"));
    }
    let expected = req.bars.checked_mul(2).unwrap_or(i64::MAX);
    if req.data.len() as i64 != expected {
        return Err(ApiError::validation(
            "data",
            format!(
                "data length {} does not match 2 * bars ({expected})",
                req.data.len()
            ),
        ));
    }
    Ok(())
}

/// Upserts the waveform for `(subject_kind, subject_id)` and returns it.
pub async fn upsert(pool: &SqlitePool, req: &UpsertWaveformRequest) -> ApiResult<WaveformDto> {
    validate(req)?;
    let now = now_ts();
    sqlx::query(
        "INSERT INTO waveforms (subject_kind, subject_id, scheme, algorithm, ms_per_bar, bars, \
         data, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(subject_kind, subject_id) DO UPDATE SET \
           scheme = excluded.scheme, algorithm = excluded.algorithm, \
           ms_per_bar = excluded.ms_per_bar, bars = excluded.bars, data = excluded.data",
    )
    .bind(&req.subject_kind)
    .bind(&req.subject_id)
    .bind(req.scheme)
    .bind(&req.algorithm)
    .bind(req.ms_per_bar)
    .bind(req.bars)
    .bind(&req.data)
    .bind(&now)
    .execute(pool)
    .await?;
    get(pool, &req.subject_kind, &req.subject_id).await
}

/// Fetches one waveform or `404`.
pub async fn get(
    pool: &SqlitePool,
    subject_kind: &str,
    subject_id: &str,
) -> ApiResult<WaveformDto> {
    let sql = format!("{SELECT} WHERE subject_kind = ? AND subject_id = ?");
    sqlx::query_as::<_, WaveformRow>(&sql)
        .bind(subject_kind)
        .bind(subject_id)
        .fetch_optional(pool)
        .await?
        .map(WaveformRow::into_dto)
        .ok_or_else(|| ApiError::not_found("waveform", subject_id))
}
