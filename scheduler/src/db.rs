//! Database bootstrap: connection options + migrations.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

/// Default on-disk database path (see consolidated-architecture §3.1).
pub const DEFAULT_DB_PATH: &str = "/data/zukka.db";

/// Connect to the SQLite database and run all pending migrations.
///
/// Connection settings per §3.1: WAL journal, foreign keys ON, 5s busy
/// timeout, synchronous NORMAL.
pub async fn connect(path: impl AsRef<Path>) -> anyhow::Result<SqlitePool> {
    let path = path.as_ref();
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .synchronous(SqliteSynchronous::Normal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
