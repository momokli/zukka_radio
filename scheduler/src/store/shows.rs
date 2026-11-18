//! Show store: read/write queries for `shows` (Issue #11).
//!
//! `shows.recurrence` is stored as the canonical rule string and parsed on
//! read. `shows` has no `version` column, so PATCH is a plain COALESCE patch
//! (schema wins, see plan R1).

use serde_json::Value;
use sqlx::SqlitePool;

use crate::dto::{CreateShowRequest, ShowDto, UpdateShowRequest};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
use crate::rrule::RecurrenceRule;
use crate::store::now_ts;
use crate::uuid::new_id;

/// Row shape of `shows`, mapped onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct ShowRow {
    id: String,
    slug: String,
    name: String,
    description: String,
    artwork_url: Option<String>,
    links: String,
    recurrence: Option<String>,
    caster_id: Option<String>,
    created_by: String,
    created_at: String,
    updated_at: String,
}

impl ShowRow {
    fn into_dto(self) -> ApiResult<ShowDto> {
        let links: Vec<Value> = serde_json::from_str(&self.links).map_err(|err| {
            ApiError::Internal(anyhow::anyhow!(
                "invalid stored show links for {}: {err}",
                self.id
            ))
        })?;
        let recurrence = parse_stored_rule(self.id.as_str(), self.recurrence)?;
        Ok(ShowDto {
            id: self.id,
            slug: self.slug,
            name: self.name,
            description: self.description,
            artwork_url: self.artwork_url,
            links,
            recurrence,
            caster_id: self.caster_id,
            created_by: self.created_by,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn parse_stored_rule(id: &str, raw: Option<String>) -> ApiResult<Option<RecurrenceRule>> {
    match raw {
        None => Ok(None),
        Some(raw) => RecurrenceRule::parse("recurrence", &raw)
            .map(Some)
            .map_err(|err| {
                ApiError::Internal(anyhow::anyhow!(
                    "invalid stored recurrence for show {id}: {err}"
                ))
            }),
    }
}

const SELECT: &str = "SELECT id, slug, name, description, artwork_url, links, recurrence, \
     caster_id, created_by, created_at, updated_at FROM shows";

/// Lists shows, optionally filtered by `caster_id`.
pub async fn list(pool: &SqlitePool, caster_id: Option<&str>) -> ApiResult<Vec<ShowDto>> {
    let sql = format!("{SELECT} WHERE (? IS NULL OR caster_id = ?) ORDER BY created_at, id");
    let rows: Vec<ShowRow> = sqlx::query_as(&sql)
        .bind(caster_id)
        .bind(caster_id)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(ShowRow::into_dto).collect()
}

/// Fetches one show or returns `404`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<ShowDto> {
    fetch(pool, id).await
}

async fn fetch(pool: &SqlitePool, id: &str) -> ApiResult<ShowDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<ShowRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    match row {
        Some(row) => row.into_dto(),
        None => Err(ApiError::not_found("show", id)),
    }
}

/// Creates a show. Validates slug/name, recurrence and referenced casters.
pub async fn create(pool: &SqlitePool, req: &CreateShowRequest) -> ApiResult<ShowDto> {
    let slug = trimmed_non_empty("slug", &req.slug)?;
    let name = trimmed_non_empty("name", &req.name)?;

    let recurrence = match &req.recurrence {
        Some(raw) => Some(RecurrenceRule::parse("recurrence", raw)?.to_string()),
        None => None,
    };
    let links = serde_json::to_string(&req.links.clone().unwrap_or_default())
        .map_err(|err| ApiError::Internal(anyhow::anyhow!("serialising links: {err}")))?;

    if let Some(caster_id) = &req.caster_id {
        ensure_caster(pool, caster_id).await?;
    }
    ensure_caster(pool, &req.created_by).await?;

    let id = new_id();
    let ts = now_ts();
    let description = req.description.clone().unwrap_or_default();

    let res = sqlx::query(
        "INSERT INTO shows \
         (id, slug, name, description, artwork_url, links, recurrence, caster_id, created_by, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&slug)
    .bind(&name)
    .bind(&description)
    .bind(req.artwork_url.as_deref())
    .bind(&links)
    .bind(recurrence.as_deref())
    .bind(req.caster_id.as_deref())
    .bind(&req.created_by)
    .bind(&ts)
    .bind(&ts)
    .execute(pool)
    .await;

    if let Err(err) = res {
        return Err(map_write_error(err, &req.created_by).await);
    }

    fetch(pool, &id).await
}

/// Updates a show (COALESCE patch, no optimistic lock).
pub async fn update(pool: &SqlitePool, id: &str, req: &UpdateShowRequest) -> ApiResult<ShowDto> {
    let slug = match &req.slug {
        Some(raw) => Some(trimmed_non_empty("slug", raw)?),
        None => None,
    };
    let name = match &req.name {
        Some(raw) => Some(trimmed_non_empty("name", raw)?),
        None => None,
    };
    let links = match &req.links {
        Some(links) => Some(
            serde_json::to_string(links)
                .map_err(|err| ApiError::Internal(anyhow::anyhow!("serialising links: {err}")))?,
        ),
        None => None,
    };
    let recurrence = match &req.recurrence {
        Some(Some(raw)) => Some(RecurrenceRule::parse("recurrence", raw)?.to_string()),
        Some(None) | None => None,
    };
    let caster_id = req.caster_id.clone().flatten();
    if let Some(caster_id) = &caster_id {
        ensure_caster(pool, caster_id).await?;
    }

    let recurrence_set = req.recurrence.is_some();
    let artwork_set = req.artwork_url.is_some();
    let caster_set = req.caster_id.is_some();
    let artwork_url = req.artwork_url.clone().flatten();
    let ts = now_ts();

    let res = sqlx::query(
        "UPDATE shows SET \
         slug = COALESCE(?, slug), \
         name = COALESCE(?, name), \
         description = COALESCE(?, description), \
         artwork_url = CASE WHEN ? THEN ? ELSE artwork_url END, \
         links = COALESCE(?, links), \
         recurrence = CASE WHEN ? THEN ? ELSE recurrence END, \
         caster_id = CASE WHEN ? THEN ? ELSE caster_id END, \
         updated_at = ? \
         WHERE id = ?",
    )
    .bind(slug)
    .bind(name)
    .bind(req.description.clone())
    .bind(artwork_set)
    .bind(artwork_url)
    .bind(links)
    .bind(recurrence_set)
    .bind(recurrence)
    .bind(caster_set)
    .bind(caster_id)
    .bind(&ts)
    .bind(id)
    .execute(pool)
    .await;

    let res = match res {
        Ok(res) => res,
        Err(err) => return Err(map_write_error(err, "").await),
    };

    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("show", id));
    }
    fetch(pool, id).await
}

/// Deletes a show. `broadcasts.show_id`/`tapes.show_id` become `NULL` (FK).
pub async fn delete(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let res = sqlx::query("DELETE FROM shows WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("show", id));
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
async fn map_write_error(err: sqlx::Error, created_by: &str) -> ApiError {
    if let Some(msg) = unique_violation(&err) {
        if msg.contains("shows.slug") {
            return ApiError::conflict("duplicate_slug", "a show with this slug already exists");
        }
    }
    if is_foreign_key_violation(&err) {
        return ApiError::not_found("caster", created_by.to_string());
    }
    err.into()
}
