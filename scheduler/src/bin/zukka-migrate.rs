//! `zukka-migrate` — legacy data migration CLI (Issue #46).
//!
//! Reads a documented JSON export of the legacy AzuraCast/Funkwhale data and
//! imports casters, tracks and tapes into `zukka.db`.
//!
//! ```text
//! zukka-migrate --legacy export.json --db /data/zukka.db            # dry-run (default)
//! zukka-migrate --legacy export.json --db /data/zukka.db --apply    # import
//! ```
//!
//! Exit codes: `0` ok, `1` validation/orphan error, `2` I/O or database error.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use zukka_scheduler::migrate::{Config, MigrateError, run};

#[derive(Debug, Parser)]
#[command(
    name = "zukka-migrate",
    about = "Import the legacy AzuraCast/Funkwhale export into zukka.db (dry-run by default)"
)]
struct Cli {
    /// Path to the legacy JSON export.
    #[arg(long, value_name = "FILE")]
    legacy: PathBuf,

    /// Path to the target zukka.db.
    #[arg(long, value_name = "DB")]
    db: PathBuf,

    /// Validate and count only; never write (default).
    #[arg(long, conflicts_with = "apply")]
    dry_run: bool,

    /// Back up the database and import in a single transaction.
    #[arg(long)]
    apply: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // `--dry-run` is the default; `--apply` opts in. clap already rejects
    // `--dry-run --apply` together.
    let apply = cli.apply && !cli.dry_run;

    let config = Config {
        legacy: cli.legacy,
        db: cli.db,
        apply,
    };

    match run(config).await {
        Ok(outcome) => {
            println!(
                "zukka-migrate: {} ({})",
                if outcome.applied {
                    "applied"
                } else {
                    "dry-run"
                },
                outcome
                    .backup
                    .as_ref()
                    .map(|b| format!("backup {}", b.display()))
                    .unwrap_or_else(|| "no writes".to_string())
            );
            print!("{}", outcome.report.render(outcome.applied));
            ExitCode::from(0)
        }
        Err(err) => {
            eprintln!("zukka-migrate: {err}");
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

#[allow(dead_code)]
fn assert_error_exit_codes(err: &MigrateError) -> i32 {
    err.exit_code()
}
