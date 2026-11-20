//! Read-only database bootstrap.
//!
//! The tapes service opens the shared `zukka.db` in SQLite `mode=ro` and runs
//! no migrations (P2, `docs/tapes.md` D4). `journal_mode`/`synchronous` are
//! deliberately **not** set: SQLite rejects both on a read-only handle with
//! `SQLITE_READONLY`. Only `busy_timeout` + `foreign_keys` are applied.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

/// Opens `path` read-only and returns a small pool.
///
/// The pool keeps at most four connections; every statement issued through it
/// is rejected by SQLite with `SQLITE_READONLY` if it attempts a write, which
/// the tests assert to prove the handle is truly read-only.
pub async fn connect_read_only(path: impl AsRef<Path>) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;

    Ok(pool)
}
