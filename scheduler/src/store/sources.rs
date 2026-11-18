//! Source store: read-mostly queries for the fixed priority layers.
//!
//! C77: the layer set is fixed — no inserts, no deletes.
//! C78: `priority`, `name`, `kind`, `id`, `created_at` are never written.

use sqlx::SqlitePool;

use crate::dto::{SourceDto, SourceLayer, UpdateSourceRequest};
use crate::error::{ApiError, ApiResult};
use crate::store::now_ts;

/// Raw row shape of `sources`; parsed into [`SourceDto`] via `TryFrom`.
#[derive(Debug, sqlx::FromRow)]
struct SourceRow {
    id: String,
    name: String,
    kind: String,
    priority: i64,
    enabled: i64,
    config: String,
    created_at: String,
    updated_at: String,
}

impl TryFrom<SourceRow> for SourceDto {
    type Error = ApiError;

    fn try_from(row: SourceRow) -> Result<Self, Self::Error> {
        let name = SourceLayer::parse("name", &row.name)?;
        let kind = SourceLayer::parse("kind", &row.kind)?;
        let enabled = match row.enabled {
            0 => false,
            1 => true,
            other => {
                return Err(ApiError::Internal(anyhow::anyhow!(
                    "sources.enabled out of range: {other}"
                )));
            }
        };
        let config = serde_json::from_str(&row.config).map_err(|err| {
            ApiError::Internal(anyhow::anyhow!("sources.config is not valid JSON: {err}"))
        })?;
        Ok(SourceDto {
            id: row.id,
            name,
            kind,
            priority: row.priority,
            enabled,
            config,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

const SELECT: &str =
    "SELECT id, name, kind, priority, enabled, config, created_at, updated_at FROM sources";

/// Lists all layers, `priority DESC, name ASC` (§2.4).
pub async fn list(pool: &SqlitePool) -> ApiResult<Vec<SourceDto>> {
    let sql = format!("{SELECT} ORDER BY priority DESC, name ASC");
    let rows: Vec<SourceRow> = sqlx::query_as(&sql).fetch_all(pool).await?;
    rows.into_iter().map(SourceDto::try_from).collect()
}

/// Lists only enabled layers (input for the pure layer selection).
pub async fn list_enabled(pool: &SqlitePool) -> ApiResult<Vec<SourceDto>> {
    let sql = format!("{SELECT} WHERE enabled = 1 ORDER BY priority DESC, name ASC");
    let rows: Vec<SourceRow> = sqlx::query_as(&sql).fetch_all(pool).await?;
    rows.into_iter().map(SourceDto::try_from).collect()
}

/// Fetches one layer or returns `404 not_found`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<SourceDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<SourceRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    match row {
        Some(row) => SourceDto::try_from(row),
        None => Err(ApiError::not_found("source", id)),
    }
}

/// Patches `enabled` and/or `config`; nothing else is writable (C78).
///
/// An empty body, a `null` config or a missing row are rejected with
/// `422` / `422` / `404` respectively.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    req: &UpdateSourceRequest,
) -> ApiResult<SourceDto> {
    let enabled = req.enabled.map(|value| i64::from(value));

    let config = match &req.config {
        // Field absent: keep the existing value.
        None => None,
        // Explicit `null`: reject (§4.6 validation_error).
        Some(None) => {
            return Err(ApiError::validation(
                "config",
                "config must be a JSON object, not null",
            ));
        }
        // Replace with the stored JSON text.
        Some(Some(value)) => Some(serde_json::to_string(value).map_err(|err| {
            ApiError::Internal(anyhow::anyhow!("failed to serialize config: {err}"))
        })?),
    };

    if enabled.is_none() && config.is_none() {
        return Err(ApiError::validation(
            "body",
            "at least one of 'enabled' or 'config' is required",
        ));
    }

    let ts = now_ts();
    let sql = "UPDATE sources SET \
               enabled = COALESCE(?, enabled), \
               config = COALESCE(?, config), \
               updated_at = ? \
               WHERE id = ?";
    let res = sqlx::query(sql)
        .bind(enabled)
        .bind(config)
        .bind(&ts)
        .bind(id)
        .execute(pool)
        .await?;

    if res.rows_affected() == 0 {
        // Either the row is missing, or the patch was a no-op write. Distinguish
        // by probing existence: a missing row is `404`, otherwise return it.
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sources WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
        if exists.is_none() {
            return Err(ApiError::not_found("source", id));
        }
    }

    get(pool, id).await
}
