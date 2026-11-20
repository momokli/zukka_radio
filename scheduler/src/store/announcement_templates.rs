//! Announcement-template store (Issue #34).
//!
//! CRUD for `announcement_templates`. A template is a **global** default
//! (`show_id IS NULL`) or a **per-show** override. Two partial unique indexes
//! enforce at most one template per `(event, channel)` and level; a collision
//! maps to `409 duplicate_template`. `shows` has no `version` column, so PATCH
//! is a COALESCE patch (schema wins).

use serde_json::json;
use sqlx::SqlitePool;

use crate::dto::{
    AnnouncementChannel, AnnouncementEvent, AnnouncementTemplateDto,
    CreateAnnouncementTemplateRequest, UpdateAnnouncementTemplateRequest,
};
use crate::error::{ApiError, ApiResult, is_foreign_key_violation, unique_violation};
use crate::store::now_ts;
use crate::uuid::new_id;

/// Row shape of `announcement_templates`, mapped onto the DTO.
#[derive(Debug, sqlx::FromRow)]
struct TemplateRow {
    id: String,
    name: String,
    event: AnnouncementEvent,
    show_id: Option<String>,
    channel: AnnouncementChannel,
    body: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl From<TemplateRow> for AnnouncementTemplateDto {
    fn from(row: TemplateRow) -> Self {
        AnnouncementTemplateDto {
            id: row.id,
            name: row.name,
            event: row.event,
            show_id: row.show_id,
            channel: row.channel,
            body: row.body,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

const SELECT: &str = "SELECT id, name, event, show_id, channel, body, enabled, created_at, \
     updated_at FROM announcement_templates";

/// Lists templates, optionally filtered by `event` and/or `show_id`.
pub async fn list(
    pool: &SqlitePool,
    event: Option<&str>,
    show_id: Option<&str>,
) -> ApiResult<Vec<AnnouncementTemplateDto>> {
    let sql = format!(
        "{SELECT} WHERE (? IS NULL OR event = ?) AND (? IS NULL OR show_id = ?) \
         ORDER BY created_at, id"
    );
    let rows: Vec<TemplateRow> = sqlx::query_as(&sql)
        .bind(event)
        .bind(event)
        .bind(show_id)
        .bind(show_id)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(AnnouncementTemplateDto::from)
        .collect())
}

/// Fetches one template or returns `404`.
pub async fn get(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementTemplateDto> {
    fetch(pool, id).await
}

async fn fetch(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementTemplateDto> {
    let sql = format!("{SELECT} WHERE id = ?");
    let row: Option<TemplateRow> = sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?;
    row.map(AnnouncementTemplateDto::from)
        .ok_or_else(|| ApiError::not_found("template", id))
}

/// Creates a template. Validates name/body/enums and the referenced show.
pub async fn create(
    pool: &SqlitePool,
    req: &CreateAnnouncementTemplateRequest,
) -> ApiResult<AnnouncementTemplateDto> {
    let name = validate_name(&req.name)?;
    let body = validate_body(&req.body)?;
    let event = AnnouncementEvent::parse("event", &req.event)?;
    let channel = match req.channel.as_deref() {
        None => AnnouncementChannel::Telegram,
        Some(raw) => AnnouncementChannel::parse("channel", raw)?,
    };
    let enabled = req.enabled.unwrap_or(true);
    if let Some(show_id) = &req.show_id {
        ensure_show(pool, show_id).await?;
    }

    let id = new_id();
    let ts = now_ts();
    let res = sqlx::query(
        "INSERT INTO announcement_templates \
         (id, name, event, show_id, channel, body, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(event.as_str())
    .bind(req.show_id.as_deref())
    .bind(channel.as_str())
    .bind(&body)
    .bind(enabled)
    .bind(&ts)
    .bind(&ts)
    .execute(pool)
    .await;

    if let Err(err) = res {
        return Err(map_write_error(err, event, channel, req.show_id.as_deref()));
    }
    fetch(pool, &id).await
}

/// Updates a template (COALESCE patch). A PATCH may move a template into an
/// occupied scope, so unique conflicts are mapped here too.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    req: &UpdateAnnouncementTemplateRequest,
) -> ApiResult<AnnouncementTemplateDto> {
    let name = match &req.name {
        Some(raw) => Some(validate_name(raw)?),
        None => None,
    };
    let body = match &req.body {
        Some(raw) => Some(validate_body(raw)?),
        None => None,
    };
    let event = match &req.event {
        Some(raw) => Some(AnnouncementEvent::parse("event", raw)?),
        None => None,
    };
    let channel = match &req.channel {
        Some(raw) => Some(AnnouncementChannel::parse("channel", raw)?),
        None => None,
    };
    let show_id = req.show_id.clone().flatten();
    if let Some(show_id) = &show_id {
        ensure_show(pool, show_id).await?;
    }
    let show_set = req.show_id.is_some();
    let ts = now_ts();

    let res = sqlx::query(
        "UPDATE announcement_templates SET \
         name = COALESCE(?, name), \
         event = COALESCE(?, event), \
         show_id = CASE WHEN ? THEN ? ELSE show_id END, \
         channel = COALESCE(?, channel), \
         body = COALESCE(?, body), \
         enabled = COALESCE(?, enabled), \
         updated_at = ? \
         WHERE id = ?",
    )
    .bind(name)
    .bind(event.map(AnnouncementEvent::as_str))
    .bind(show_set)
    .bind(show_id.as_deref())
    .bind(channel.map(AnnouncementChannel::as_str))
    .bind(body)
    .bind(req.enabled)
    .bind(&ts)
    .bind(id)
    .execute(pool)
    .await;

    let res = match res {
        Ok(res) => res,
        Err(err) => {
            let event = match event {
                Some(event) => event,
                None => current_event(pool, id).await?,
            };
            let channel = match channel {
                Some(channel) => channel,
                None => current_channel(pool, id).await?,
            };
            return Err(map_write_error(err, event, channel, show_id.as_deref()));
        }
    };

    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("template", id));
    }
    fetch(pool, id).await
}

async fn current_event(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementEvent> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT event FROM announcement_templates WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    match raw {
        Some(raw) => AnnouncementEvent::parse("event", &raw).map_err(|_| stored_corrupt(id)),
        None => Err(ApiError::not_found("template", id)),
    }
}

async fn current_channel(pool: &SqlitePool, id: &str) -> ApiResult<AnnouncementChannel> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT channel FROM announcement_templates WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    match raw {
        Some(raw) => AnnouncementChannel::parse("channel", &raw).map_err(|_| stored_corrupt(id)),
        None => Err(ApiError::not_found("template", id)),
    }
}

fn stored_corrupt(id: &str) -> ApiError {
    ApiError::Internal(anyhow::anyhow!("invalid stored enum for template {id}"))
}

/// Deletes a template; `404` when the id is unknown.
pub async fn delete(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let res = sqlx::query("DELETE FROM announcement_templates WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("template", id));
    }
    Ok(())
}

fn validate_name(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::validation("name", "name must not be empty"));
    }
    if trimmed.chars().count() > 120 {
        return Err(ApiError::validation(
            "name",
            "name must be at most 120 characters",
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_body(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::validation("body", "body must not be empty"));
    }
    if trimmed.chars().count() > 4096 {
        return Err(ApiError::validation(
            "body",
            "body must be at most 4096 characters",
        ));
    }
    Ok(trimmed.to_string())
}

async fn ensure_show(pool: &SqlitePool, id: &str) -> ApiResult<()> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shows WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("show", id.to_string()));
    }
    Ok(())
}

/// Maps SQLite constraint errors onto the §4.6 envelope.
fn map_write_error(
    err: sqlx::Error,
    event: AnnouncementEvent,
    channel: AnnouncementChannel,
    show_id: Option<&str>,
) -> ApiError {
    if unique_violation(&err).is_some() {
        let scope = match show_id {
            Some(show_id) => format!("show {show_id}"),
            None => "the global scope".to_string(),
        };
        return ApiError::Conflict {
            code: "duplicate_template",
            message: format!(
                "a {} template for channel {} already exists for {scope}",
                event.as_str(),
                channel.as_str()
            ),
            details: Some(json!({
                "event": event.as_str(),
                "channel": channel.as_str(),
                "show_id": show_id,
            })),
        };
    }
    if is_foreign_key_violation(&err) {
        if let Some(show_id) = show_id {
            return ApiError::not_found("show", show_id.to_string());
        }
    }
    err.into()
}
