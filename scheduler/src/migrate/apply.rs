//! Apply path: backup, single transaction, idempotent upserts, FK check
//! (Issue #46, Story 6).
//!
//! The import runs in FK order (casters → tracks → tapes). Rows were already
//! classified as insert/update by [`crate::migrate::plan`], so the counters in
//! the report match exactly what this module writes.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::migrate::map::{CasterRow, TapeRow, TrackRow};

/// Failure modes of the apply step.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// `PRAGMA foreign_key_check` reported rows after the import.
    #[error("foreign_key_check reported {0} row(s) after import; rolled back")]
    ForeignKeyCheck(usize),
    /// Any database or filesystem failure.
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ApplyError {
    fn from(err: sqlx::Error) -> Self {
        ApplyError::Db(anyhow::Error::from(err))
    }
}

/// The write plan: rows to insert and rows to update, per entity.
#[derive(Debug, Default)]
pub struct WritePlan {
    pub caster_inserts: Vec<CasterRow>,
    pub caster_updates: Vec<CasterRow>,
    pub track_inserts: Vec<TrackRow>,
    pub track_updates: Vec<TrackRow>,
    pub tape_inserts: Vec<TapeRow>,
    pub tape_updates: Vec<TapeRow>,
}

impl WritePlan {
    /// Total number of rows the plan would write.
    pub fn total(&self) -> usize {
        self.caster_inserts.len()
            + self.caster_updates.len()
            + self.track_inserts.len()
            + self.track_updates.len()
            + self.tape_inserts.len()
            + self.tape_updates.len()
    }
}

/// Creates `<db>.bak` via `VACUUM INTO` without overwriting an existing backup.
pub async fn backup(pool: &SqlitePool, db: &Path) -> Result<PathBuf, ApplyError> {
    let bak = PathBuf::from(format!("{}.bak", db.display()));
    if bak.exists() {
        return Err(ApplyError::Db(anyhow::anyhow!(
            "backup already exists, refusing to overwrite: {}",
            bak.display()
        )));
    }
    let escaped = bak.display().to_string().replace('\'', "''");
    sqlx::query(&format!("VACUUM INTO '{escaped}'"))
        .execute(pool)
        .await?;
    Ok(bak)
}

/// Applies the write plan in one transaction and verifies FK integrity before
/// committing.
pub async fn apply(pool: &SqlitePool, plan: &WritePlan) -> Result<(), ApplyError> {
    let mut tx = pool.begin().await?;
    {
        let conn: &mut SqliteConnection = &mut tx;

        for row in &plan.caster_inserts {
            insert_caster(conn, row).await?;
        }
        for row in &plan.caster_updates {
            update_caster(conn, row).await?;
        }
        for row in &plan.track_inserts {
            insert_track(conn, row).await?;
        }
        for row in &plan.track_updates {
            update_track(conn, row).await?;
        }
        for row in &plan.tape_inserts {
            insert_tape(conn, row).await?;
        }
        for row in &plan.tape_updates {
            update_tape(conn, row).await?;
        }

        let fk_rows = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await?;
        if !fk_rows.is_empty() {
            let n = fk_rows.len();
            tx.rollback().await?;
            return Err(ApplyError::ForeignKeyCheck(n));
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn insert_caster(conn: &mut SqliteConnection, row: &CasterRow) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO casters \
         (id, username, display_name, email, password_hash, role_id, is_active, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.id)
    .bind(&row.username)
    .bind(&row.display_name)
    .bind(&row.email)
    .bind(&row.password_hash)
    .bind(&row.role_id)
    .bind(row.is_active)
    .bind(&row.created_at)
    .bind(&row.updated_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn update_caster(conn: &mut SqliteConnection, row: &CasterRow) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE casters SET username = ?, display_name = ?, email = ?, password_hash = ?, \
         role_id = ?, is_active = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&row.username)
    .bind(&row.display_name)
    .bind(&row.email)
    .bind(&row.password_hash)
    .bind(&row.role_id)
    .bind(row.is_active)
    .bind(&row.updated_at)
    .bind(&row.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn insert_track(conn: &mut SqliteConnection, row: &TrackRow) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO tracks \
         (id, music_api_id, isrc, title, artist, album, duration_ms, storage_key, \
          visibility, caster_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.id)
    .bind(&row.music_api_id)
    .bind(&row.isrc)
    .bind(&row.title)
    .bind(&row.artist)
    .bind(&row.album)
    .bind(row.duration_ms)
    .bind(&row.storage_key)
    .bind(&row.visibility)
    .bind(&row.caster_id)
    .bind(&row.created_at)
    .bind(&row.updated_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn update_track(conn: &mut SqliteConnection, row: &TrackRow) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tracks SET music_api_id = ?, isrc = ?, title = ?, artist = ?, album = ?, \
         duration_ms = ?, storage_key = ?, visibility = ?, caster_id = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&row.music_api_id)
    .bind(&row.isrc)
    .bind(&row.title)
    .bind(&row.artist)
    .bind(&row.album)
    .bind(row.duration_ms)
    .bind(&row.storage_key)
    .bind(&row.visibility)
    .bind(&row.caster_id)
    .bind(&row.updated_at)
    .bind(&row.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn insert_tape(conn: &mut SqliteConnection, row: &TapeRow) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO tapes \
         (id, slug, caster_id, title, started_at, ended_at, duration_ms, source, \
          music_api_id, storage_key, format, size_bytes, status, visible, \
          created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.id)
    .bind(&row.slug)
    .bind(&row.caster_id)
    .bind(&row.title)
    .bind(&row.started_at)
    .bind(&row.ended_at)
    .bind(row.duration_ms)
    .bind(&row.source)
    .bind(&row.music_api_id)
    .bind(&row.storage_key)
    .bind(&row.format)
    .bind(row.size_bytes)
    .bind(&row.status)
    .bind(row.visible)
    .bind(&row.created_at)
    .bind(&row.updated_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn update_tape(conn: &mut SqliteConnection, row: &TapeRow) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tapes SET slug = ?, caster_id = ?, title = ?, started_at = ?, ended_at = ?, \
         duration_ms = ?, source = ?, music_api_id = ?, storage_key = ?, format = ?, \
         size_bytes = ?, status = ?, visible = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&row.slug)
    .bind(&row.caster_id)
    .bind(&row.title)
    .bind(&row.started_at)
    .bind(&row.ended_at)
    .bind(row.duration_ms)
    .bind(&row.source)
    .bind(&row.music_api_id)
    .bind(&row.storage_key)
    .bind(&row.format)
    .bind(row.size_bytes)
    .bind(&row.status)
    .bind(row.visible)
    .bind(&row.updated_at)
    .bind(&row.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
