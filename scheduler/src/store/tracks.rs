//! Track store (Issue #19 "Library"): read/write/search queries for `tracks`.
//!
//! The scheduler is the single writer of `tracks` (§4.2, P2). The portal reaches
//! these functions only through `/api/v1/tracks*` with a service token and a
//! viewer context (`X-Caster-Id`).
//!
//! Visibility (§1, single source of truth):
//!
//! ```text
//! visible(track, viewer) := track.visibility = 'shared' OR track.caster_id = viewer
//! ```
//!
//! The scope is in every read WHERE clause (list/search/detail) so a foreign
//! private track is invisible in search results *and* via a direct URL — a
//! `404`, never a `403` (existence is not revealed, §4.5). `viewer = None`
//! (no header) means only `shared` tracks are visible.

use serde_json::Value;
use sqlx::SqlitePool;

use crate::dto::{CreateTrackRequest, TrackDto, UpdateTrackRequest, Visibility};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
use crate::store::now_ts;
use crate::uuid::new_id;

/// Row shape of `tracks`, mapped onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct TrackRow {
    id: String,
    music_api_id: String,
    isrc: Option<String>,
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
    storage_key: String,
    artwork_url: Option<String>,
    visibility: String,
    caster_id: Option<String>,
    metadata: String,
    created_at: String,
    updated_at: String,
}

impl TrackRow {
    fn into_dto(self) -> ApiResult<TrackDto> {
        let metadata: Value = serde_json::from_str(&self.metadata).map_err(|err| {
            ApiError::Internal(anyhow::anyhow!(
                "invalid stored track metadata for {}: {err}",
                self.id
            ))
        })?;
        let visibility = Visibility::parse("visibility", &self.visibility).map_err(|err| {
            ApiError::Internal(anyhow::anyhow!(
                "invalid stored visibility for track {}: {err}",
                self.id
            ))
        })?;
        Ok(TrackDto {
            id: self.id,
            music_api_id: self.music_api_id,
            isrc: self.isrc,
            title: self.title,
            artist: self.artist,
            album: self.album,
            duration_ms: self.duration_ms,
            storage_key: self.storage_key,
            artwork_url: self.artwork_url,
            visibility,
            caster_id: self.caster_id,
            metadata,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "SELECT id, music_api_id, isrc, title, artist, album, duration_ms, \
     storage_key, artwork_url, visibility, caster_id, metadata, created_at, updated_at FROM tracks";

/// The visibility scope predicate (bound with `viewer` as the last parameter).
const SCOPE: &str = "(visibility = 'shared' OR caster_id = ?)";

/// Lists/searches tracks with the §1 visibility scope.
///
/// `q` does a case-insensitive `LIKE` over `title|artist|album|isrc`. Optional
/// `visibility`/`owner_caster_id` filters narrow the result further. Ordered by
/// `created_at DESC, id`.
pub async fn search(
    pool: &SqlitePool,
    q: Option<&str>,
    visibility: Option<&str>,
    owner_caster_id: Option<&str>,
    viewer: Option<&str>,
    limit: i64,
    offset: i64,
) -> ApiResult<Vec<TrackDto>> {
    let like = q.map(|q| format!("%{}%", q.trim()));
    let sql = format!(
        "{SELECT} \
         WHERE (? IS NULL OR title LIKE ? OR artist LIKE ? OR album LIKE ? OR isrc LIKE ?) \
         AND (? IS NULL OR visibility = ?) \
         AND (? IS NULL OR caster_id = ?) \
         AND {SCOPE} \
         ORDER BY created_at DESC, id LIMIT ? OFFSET ?"
    );
    let rows: Vec<TrackRow> = sqlx::query_as(&sql)
        .bind(q)
        .bind(like.as_deref())
        .bind(like.as_deref())
        .bind(like.as_deref())
        .bind(like.as_deref())
        .bind(visibility)
        .bind(visibility)
        .bind(owner_caster_id)
        .bind(owner_caster_id)
        .bind(viewer)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(TrackRow::into_dto).collect()
}

/// Fetches one track within the viewer's visibility scope, else `404`.
pub async fn get(pool: &SqlitePool, id: &str, viewer: Option<&str>) -> ApiResult<TrackDto> {
    let sql = format!("{SELECT} WHERE id = ? AND {SCOPE}");
    let row: Option<TrackRow> = sqlx::query_as(&sql)
        .bind(id)
        .bind(viewer)
        .fetch_optional(pool)
        .await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::not_found("track", id)),
    }
}

/// Unscoped fetch (internal use after a write we just performed).
async fn fetch(pool: &SqlitePool, id: &str) -> ApiResult<TrackDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<TrackRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::not_found("track", id)),
    }
}

/// Creates a track. Validates the mandatory fields, `duration_ms >= 0`;
/// `visibility` defaults to `shared`; `caster_id` is optional.
pub async fn create(pool: &SqlitePool, req: &CreateTrackRequest) -> ApiResult<TrackDto> {
    let title = trimmed_non_empty("title", &req.title)?;
    let music_api_id = trimmed_non_empty("music_api_id", &req.music_api_id)?;
    let storage_key = trimmed_non_empty("storage_key", &req.storage_key)?;
    if req.duration_ms < 0 {
        return Err(ApiError::validation(
            "duration_ms",
            "duration_ms must be >= 0",
        ));
    }
    let visibility = match &req.visibility {
        Some(raw) => Visibility::parse("visibility", raw)?,
        None => Visibility::Shared,
    };
    if let Some(caster_id) = &req.caster_id {
        ensure_caster(pool, caster_id).await?;
    }

    let metadata = serde_json::to_string(
        &req.metadata
            .clone()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    )
    .map_err(|err| ApiError::Internal(anyhow::anyhow!("serialising metadata: {err}")))?;

    let id = new_id();
    let ts = now_ts();
    let artist = req.artist.clone().unwrap_or_default();
    let album = req.album.clone().unwrap_or_default();

    let res = sqlx::query(
        "INSERT INTO tracks \
         (id, music_api_id, isrc, title, artist, album, duration_ms, storage_key, artwork_url, \
          visibility, caster_id, metadata, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&music_api_id)
    .bind(req.isrc.as_deref())
    .bind(&title)
    .bind(&artist)
    .bind(&album)
    .bind(req.duration_ms)
    .bind(&storage_key)
    .bind(req.artwork_url.as_deref())
    .bind(visibility.as_str())
    .bind(req.caster_id.as_deref())
    .bind(&metadata)
    .bind(&ts)
    .bind(&ts)
    .execute(pool)
    .await;

    if let Err(err) = res {
        return Err(map_write_error(err));
    }

    fetch(pool, &id).await
}

/// Import from Music-API fields (Issue #19, US-1). Same validation as
/// [`create`]; a duplicate `music_api_id` → `409 duplicate_track`.
pub async fn import(
    pool: &SqlitePool,
    req: &crate::dto::ImportTrackRequest,
) -> ApiResult<TrackDto> {
    create(
        pool,
        &CreateTrackRequest {
            music_api_id: req.music_api_id.clone(),
            title: req.title.clone(),
            storage_key: req.storage_key.clone(),
            duration_ms: req.duration_ms,
            artist: req.artist.clone(),
            album: req.album.clone(),
            isrc: req.isrc.clone(),
            artwork_url: req.artwork_url.clone(),
            visibility: req.visibility.clone(),
            caster_id: req.caster_id.clone(),
            metadata: None,
        },
    )
    .await
}

/// Updates a track (COALESCE patch, no optimistic lock; pattern `store/shows.rs`).
///
/// Updates are **owner-only**: the scope is `id = ? AND caster_id = ?`. A
/// non-owner — including one who can see the track because it is `shared` —
/// gets `0` affected rows and therefore a `404` (never `403`: no existence
/// leak). Search/get keep the broader visibility scope.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    req: &UpdateTrackRequest,
    viewer: Option<&str>,
) -> ApiResult<TrackDto> {
    let title = match &req.title {
        Some(raw) => Some(trimmed_non_empty("title", raw)?),
        None => None,
    };
    let visibility = match &req.visibility {
        Some(raw) => Some(Visibility::parse("visibility", raw)?),
        None => None,
    };
    let artwork_set = req.artwork_url.is_some();
    let artwork_url = req.artwork_url.clone().flatten();
    let caster_set = req.caster_id.is_some();
    let caster_id = req.caster_id.clone().flatten();
    if let Some(caster_id) = &caster_id {
        ensure_caster(pool, caster_id).await?;
    }
    let ts = now_ts();

    let res = sqlx::query(
        "UPDATE tracks SET \
         title = COALESCE(?, title), \
         artist = COALESCE(?, artist), \
         album = COALESCE(?, album), \
         artwork_url = CASE WHEN ? THEN ? ELSE artwork_url END, \
         visibility = COALESCE(?, visibility), \
         caster_id = CASE WHEN ? THEN ? ELSE caster_id END, \
         updated_at = ? \
         WHERE id = ? AND caster_id = ?",
    )
    .bind(title)
    .bind(req.artist.clone())
    .bind(req.album.clone())
    .bind(artwork_set)
    .bind(artwork_url)
    .bind(visibility.map(Visibility::as_str))
    .bind(caster_set)
    .bind(caster_id)
    .bind(&ts)
    .bind(id)
    .bind(viewer)
    .execute(pool)
    .await;

    let res = match res {
        Ok(res) => res,
        Err(err) => return Err(map_write_error(err)),
    };

    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("track", id));
    }
    get(pool, id, viewer).await
}

/// Deletes a track within the viewer's visibility scope, else `404`.
///
/// "Who may delete" is authorized by the portal via the `library.delete`
/// permission (US-7: operator/admin); the scheduler has no roles. This scope
/// only ensures a viewer cannot act on a track invisible to them — a foreign
/// private track is a `404`. Deliberately **not** owner-only: operators must be
/// able to delete foreign shared tracks.
pub async fn delete(pool: &SqlitePool, id: &str, viewer: Option<&str>) -> ApiResult<()> {
    let res =
        sqlx::query("DELETE FROM tracks WHERE id = ? AND (visibility = 'shared' OR caster_id = ?)")
            .bind(id)
            .bind(viewer)
            .execute(pool)
            .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("track", id));
    }
    Ok(())
}

fn trimmed_non_empty(field: &'static str, raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::validation(
            field,
            format!("{field} must not be empty"),
        ));
    }
    Ok(trimmed.to_string())
}

async fn ensure_caster(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM casters WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("caster", id.to_string()));
    }
    Ok(())
}

/// Maps SQLite constraint errors onto the §4.6 envelope.
fn map_write_error(err: sqlx::Error) -> ApiError {
    if let Some(msg) = unique_violation(&err) {
        if msg.contains("tracks.music_api_id") {
            return ApiError::conflict(
                "duplicate_track",
                "a track with this music_api_id already exists",
            );
        }
        if msg.contains("tracks.isrc") {
            return ApiError::conflict("duplicate_isrc", "a track with this ISRC already exists");
        }
    }
    if is_foreign_key_violation(&err) {
        return ApiError::not_found("caster", String::new());
    }
    err.into()
}
