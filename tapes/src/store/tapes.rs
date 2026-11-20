//! Tape queries. Every query is scoped to the public predicate
//! `visible = 1 AND status = 'ready'` (the single `public_tape` guard).

use chrono::{DateTime, Utc};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::cursor::Cursor;
use crate::error::{ApiError, ApiResult};
use crate::model::{CasterRefDto, PeaksDto, ShowRefDto, TapeDto};
use crate::time::{fmt_api_time, fmt_storage_time, fmt_utc_z, parse_storage_time, window_label};

/// Feed sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// `started_at DESC, slug DESC` (default).
    Newest,
    /// `started_at ASC, slug ASC`.
    Oldest,
}

impl Sort {
    /// Parses the `sort` query value; unknown values → `400`.
    pub fn parse(raw: &str) -> ApiResult<Self> {
        match raw {
            "newest" => Ok(Sort::Newest),
            "oldest" => Ok(Sort::Oldest),
            other => Err(ApiError::bad_request(format!(
                "invalid sort: expected 'newest' or 'oldest', got '{other}'"
            ))),
        }
    }

    fn descending(self) -> bool {
        matches!(self, Sort::Newest)
    }
}

/// A joined `tapes` row with its caster/show names.
#[derive(Debug, sqlx::FromRow)]
pub struct TapeRow {
    pub id: String,
    pub slug: String,
    pub caster_id: String,
    pub caster_name: String,
    pub show_id: Option<String>,
    pub show_name: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: Option<i64>,
    pub source: String,
    pub format: Option<String>,
    pub mime: Option<String>,
    pub size_bytes: Option<i64>,
    pub published_at: Option<String>,
    pub updated_at: String,
    /// Music API object key (proxy target); never serialized publicly.
    pub storage_key: Option<String>,
    /// Object ETag from the Music API metadata; never serialized publicly.
    pub etag: Option<String>,
}

/// One feed item: the public DTO plus the exact keyset position.
#[derive(Debug)]
pub struct FeedItem {
    pub dto: TapeDto,
    /// `started_at` in Unix ms (cursor key `t`).
    pub started_at_ms: i64,
}

const TAPE_SELECT: &str = "SELECT t.id, t.slug, t.caster_id, c.display_name AS caster_name, \
     t.show_id, s.name AS show_name, t.title, t.description, t.started_at, t.ended_at, \
     t.duration_ms, t.source, t.format, t.mime, t.size_bytes, t.published_at, t.updated_at, \
     t.storage_key, t.etag \
     FROM tapes t \
     JOIN casters c ON c.id = t.caster_id \
     LEFT JOIN shows s ON s.id = t.show_id \
     WHERE t.visible = 1 AND t.status = 'ready'";

/// Loads one public tape by slug, or `None` when unknown / not public.
///
/// The guard is applied in SQL, so an invisible, pending, processing, failed or
/// deleted tape is indistinguishable from a nonexistent one.
pub async fn public_tape(pool: &SqlitePool, slug: &str) -> ApiResult<Option<TapeRow>> {
    let sql = format!("{TAPE_SELECT} AND t.slug = ?");
    let row = sqlx::query_as::<_, TapeRow>(&sql)
        .bind(slug)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Feed query with filters + keyset pagination. Fetches `limit + 1` rows so the
/// caller can detect further pages.
#[allow(clippy::too_many_arguments)]
pub async fn feed(
    pool: &SqlitePool,
    base_url: &str,
    casters: &[String],
    from_utc: Option<DateTime<Utc>>,
    to_utc: Option<DateTime<Utc>>,
    sort: Sort,
    limit: i64,
    cursor: Option<&Cursor>,
) -> ApiResult<Vec<FeedItem>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(TAPE_SELECT);

    if !casters.is_empty() {
        qb.push(" AND t.caster_id IN (");
        let mut sep = qb.separated(", ");
        for caster in casters {
            sep.push_bind(caster.clone());
        }
        sep.push_unseparated(")");
    }
    if let Some(from) = from_utc {
        qb.push(" AND t.started_at >= ")
            .push_bind(fmt_storage_time(from));
    }
    if let Some(to) = to_utc {
        qb.push(" AND t.started_at < ")
            .push_bind(fmt_storage_time(to));
    }
    if let Some(cursor) = cursor {
        let ts = DateTime::<Utc>::from_timestamp_millis(cursor.t)
            .ok_or_else(|| ApiError::bad_request("invalid cursor: out-of-range timestamp"))
            .map(fmt_storage_time)?;
        if sort.descending() {
            qb.push(" AND (t.started_at < ")
                .push_bind(ts.clone())
                .push(" OR (t.started_at = ")
                .push_bind(ts)
                .push(" AND t.slug < ")
                .push_bind(cursor.s.clone())
                .push("))");
        } else {
            qb.push(" AND (t.started_at > ")
                .push_bind(ts.clone())
                .push(" OR (t.started_at = ")
                .push_bind(ts)
                .push(" AND t.slug > ")
                .push_bind(cursor.s.clone())
                .push("))");
        }
    }
    if sort.descending() {
        qb.push(" ORDER BY t.started_at DESC, t.slug DESC");
    } else {
        qb.push(" ORDER BY t.started_at ASC, t.slug ASC");
    }
    qb.push(" LIMIT ").push_bind(limit + 1);

    let rows: Vec<TapeRow> = qb.build_query_as().fetch_all(pool).await?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let started_at_ms = parse_storage_time(&row.started_at)?.timestamp_millis();
        let peaks = peaks_for(pool, &row.id).await?;
        items.push(FeedItem {
            dto: row_to_dto(&row, peaks, base_url)?,
            started_at_ms,
        });
    }
    Ok(items)
}

/// Peak metadata for a tape, or `None` when no `waveforms` row exists.
pub async fn peaks_for(pool: &SqlitePool, tape_id: &str) -> ApiResult<Option<PeaksDto>> {
    let row = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT scheme, ms_per_bar, bars FROM waveforms \
         WHERE subject_kind = 'tape' AND subject_id = ?",
    )
    .bind(tape_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(scheme, ms_per_bar, bars)| PeaksDto {
        scheme,
        ms_per_bar,
        bars,
    }))
}

/// The full waveform row (metadata + RMS bytes) for a tape.
#[derive(Debug, sqlx::FromRow)]
pub struct WaveformRow {
    pub scheme: i64,
    pub algorithm: String,
    pub ms_per_bar: i64,
    pub bars: i64,
    pub data: Vec<u8>,
}

/// Loads the waveform bytes for a tape, or `None` when absent.
///
/// Callers must have passed the [`public_tape`] guard first; this query is
/// intentionally not public-filtered itself.
pub async fn waveform_for(pool: &SqlitePool, tape_id: &str) -> ApiResult<Option<WaveformRow>> {
    let row = sqlx::query_as::<_, WaveformRow>(
        "SELECT scheme, algorithm, ms_per_bar, bars, data FROM waveforms \
         WHERE subject_kind = 'tape' AND subject_id = ?",
    )
    .bind(tape_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Builds the public [`TapeDto`] from a joined row (§3.3).
pub fn row_to_dto(row: &TapeRow, peaks: Option<PeaksDto>, base_url: &str) -> ApiResult<TapeDto> {
    let start = parse_storage_time(&row.started_at)?;
    let end = parse_storage_time(&row.ended_at)?;
    let updated = parse_storage_time(&row.updated_at)?;

    let title = row
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let show_name = row
        .show_name
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);

    let display_title = title
        .clone()
        .or_else(|| show_name.clone())
        .unwrap_or_else(|| row.caster_name.clone());

    let ext = row.format.as_deref().unwrap_or("mp3");
    let base = base_url.trim_end_matches('/');
    let audio_url = format!("{base}/media/{}.{}", row.slug, ext);
    let download_url = format!("{audio_url}?download=1");
    let waveform_url = format!("{base}/api/v1/tapes/{}/waveform", row.slug);
    let share_url = format!("{base}/t/{}", row.slug);

    let published_at = match row.published_at.as_deref() {
        Some(raw) => Some(fmt_utc_z(parse_storage_time(raw)?)),
        None => None,
    };

    Ok(TapeDto {
        slug: row.slug.clone(),
        caster: CasterRefDto {
            id: row.caster_id.clone(),
            name: row.caster_name.clone(),
            accent: None,
        },
        show: match (row.show_id.as_deref(), show_name) {
            (Some(id), Some(name)) => Some(ShowRefDto {
                id: id.to_owned(),
                name,
            }),
            _ => None,
        },
        title,
        display_title,
        description: row.description.clone(),
        started_at: fmt_utc_z(start),
        ended_at: fmt_utc_z(end),
        start_local: fmt_api_time(start),
        end_local: fmt_api_time(end),
        tz: "Europe/Berlin".to_string(),
        window_label: window_label(start, end),
        duration_ms: row.duration_ms,
        source: row.source.clone(),
        format: row.format.clone(),
        mime: row.mime.clone(),
        size_bytes: row.size_bytes,
        audio_url,
        download_url,
        waveform_url,
        peaks,
        share_url,
        published_at,
        updated_at: fmt_utc_z(updated),
    })
}
