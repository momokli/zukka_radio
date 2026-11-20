//! Test fixtures: load the scheduler schema into a fresh SQLite file.
//!
//! The tapes crate owns no migrations, so tests materialise the scheduler
//! schema (`scheduler/migrations/0001_init.sql`) directly via `include_str!`.
//! No network is used.

use std::path::Path;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The scheduler schema, embedded at compile time.
pub const SCHEMA: &str = include_str!("../../scheduler/migrations/0001_init.sql");

/// Creates a writable pool at `path` and applies the scheduler schema.
///
/// Use a **separate** read-only pool ([`crate::db::connect_read_only`]) for the
/// application under test, so writes and reads never share a handle.
pub async fn create_db(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    Ok(pool)
}
