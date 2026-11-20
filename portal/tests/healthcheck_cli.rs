//! `--healthcheck` CLI contract (distroless container probe, §3.6).

use std::process::Command;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::tempdir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_zukka-portal")
}

async fn create_db(path: &std::path::Path) {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
        )
        .await
        .expect("create db");
    sqlx::query("CREATE TABLE probe (id INTEGER)")
        .execute(&pool)
        .await
        .expect("create table");
    pool.close().await;
}

#[tokio::test]
async fn healthcheck_succeeds_with_readable_database() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("zukka.db");
    create_db(&db).await;

    let status = Command::new(binary())
        .arg("--healthcheck")
        .env("ZUKKA_DB", &db)
        .status()
        .expect("run binary");

    assert!(status.success(), "healthcheck should pass");
}

#[test]
fn healthcheck_fails_when_database_is_missing() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("missing.db");

    let status = Command::new(binary())
        .arg("--healthcheck")
        .env("ZUKKA_DB", &db)
        .status()
        .expect("run binary");

    assert!(!status.success(), "healthcheck must fail without a DB");
}
