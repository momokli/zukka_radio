//! Caster aggregate query (`docs/tapes.md` §3.4).
//!
//! Only casters with at least one public tape (`visible = 1 AND status =
//! 'ready'`) are returned, so a caster whose tapes are all hidden never leaks a
//! counter (R9). Sorting is by `display_name`; the schema has no `sort_order`.

use sqlx::SqlitePool;

use crate::error::ApiResult;
use crate::model::CasterSummaryDto;
use crate::time::{fmt_utc_z, parse_storage_time};

/// Lists public casters with `tape_count > 0`, sorted by `display_name`.
pub async fn list_public(pool: &SqlitePool) -> ApiResult<Vec<CasterSummaryDto>> {
    let rows = sqlx::query_as::<_, (String, String, i64, Option<String>)>(
        "SELECT c.id, c.display_name, COUNT(t.id) AS tape_count, MAX(t.started_at) AS last_tape_at \
         FROM casters c \
         JOIN tapes t ON t.caster_id = c.id AND t.visible = 1 AND t.status = 'ready' \
         GROUP BY c.id, c.display_name \
         HAVING COUNT(t.id) > 0 \
         ORDER BY c.display_name ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, name, tape_count, last_tape_at) in rows {
        let last_tape_at = match last_tape_at.as_deref() {
            Some(raw) => Some(fmt_utc_z(parse_storage_time(raw)?)),
            None => None,
        };
        out.push(CasterSummaryDto {
            id,
            name,
            accent: None,
            tape_count,
            last_tape_at,
        });
    }
    Ok(out)
}
