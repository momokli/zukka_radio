//! `--healthcheck` CLI contract (distroless container probe, §3.6).
//!
//! Exit 0 when the database is reachable and intact, != 0 when it is not.
//! The tests drive the real binary, exactly like the Docker `HEALTHCHECK`.

use std::process::Command;

use tempfile::tempdir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_zukka-scheduler")
}

#[test]
fn healthcheck_succeeds_with_writable_database() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("zukka.db");

    let status = Command::new(binary())
        .arg("--healthcheck")
        .env("ZUKKA_DB", &db)
        .status()
        .expect("run binary");

    assert!(status.success(), "healthcheck should pass for a fresh DB");
    assert!(db.exists(), "healthcheck must create/initialise the DB");
}

#[test]
fn healthcheck_fails_when_database_is_unreachable() {
    let dir = tempdir().expect("tempdir");
    // Parent directory does not exist → SQLite cannot create the file.
    let db = dir.path().join("missing").join("zukka.db");

    let status = Command::new(binary())
        .arg("--healthcheck")
        .env("ZUKKA_DB", &db)
        .status()
        .expect("run binary");

    assert!(!status.success(), "healthcheck must fail without a DB");
}
