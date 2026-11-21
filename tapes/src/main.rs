//! Zukka Radio tapes service (`zukka-tapes`).
//!
//! Two subcommands share the same binary and configuration:
//!
//! - `serve` (default) — the public read-only HTTP API.
//! - `work` — the waveform worker; requires Music API + scheduler tokens and
//!   both ffmpeg binaries, and never touches the database.

use anyhow::Context;
use clap::{Parser, Subcommand};

use zukka_tapes::config::Config;
use zukka_tapes::db;

#[derive(Debug, Parser)]
#[command(
    name = "zukka-tapes",
    version,
    about = "Zukka Radio public Tapes service"
)]
struct Cli {
    /// Distroless healthcheck: verify the shared database is readable, exit.
    #[arg(long)]
    healthcheck: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Public read-only HTTP API (default).
    Serve,
    /// Waveform worker (probe/peaks jobs from the scheduler).
    Work,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let config = Config::from_env();

    if cli.healthcheck {
        if let Err(err) = healthcheck(&config).await {
            eprintln!("healthcheck failed: {err:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await,
        Command::Work => work(config).await,
    }
}

/// `--healthcheck` implementation: mirrors `GET /api/v1/healthz`.
async fn healthcheck(config: &Config) -> anyhow::Result<()> {
    let pool = db::connect_read_only(&config.database_path)
        .await
        .with_context(|| {
            format!(
                "failed to open database read-only at {}",
                config.database_path.display()
            )
        })?;
    sqlx::query("SELECT 1 FROM tapes LIMIT 1")
        .fetch_optional(&pool)
        .await
        .context("database check failed")?;
    pool.close().await;
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = db::connect_read_only(&config.database_path)
        .await
        .with_context(|| {
            format!(
                "failed to open database read-only at {}",
                config.database_path.display()
            )
        })?;
    tracing::info!(
        "database opened read-only at {}",
        config.database_path.display()
    );

    let app = zukka_tapes::api::router(
        pool,
        config.base_url.clone(),
        config.music_api_url.clone(),
        config.music_api_token.clone(),
        config.scheduler_url.clone(),
        config.scheduler_token.clone(),
    );

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("zukka-tapes listening on {}", config.bind);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn work(config: Config) -> anyhow::Result<()> {
    zukka_tapes::worker::check_prerequisites(&config)
        .map_err(|err| anyhow::anyhow!("worker prerequisites failed: {err}"))?;
    tracing::info!(
        "starting waveform worker (scheduler {}, music api {})",
        config.scheduler_url,
        config.music_api_url
    );
    zukka_tapes::worker::build(&config)
        .run()
        .await
        .map_err(|err| anyhow::anyhow!("worker stopped: {err}"))
}
