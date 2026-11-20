use std::time::Duration;

use std::sync::Arc;

use anyhow::Context;

use zukka_portal::api_client::HttpIdentityApi;
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Distroless container healthcheck (§3.6): the portal holds no database
    // connection of its own (P2), so readiness is "shared DB readable". No
    // server config is required, which keeps the probe usable in a bare image.
    if std::env::args().any(|arg| arg == "--healthcheck") {
        if let Err(err) = healthcheck().await {
            eprintln!("healthcheck failed: {err:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let config = Config::from_env().context("invalid portal configuration")?;
    let identity = HttpIdentityApi::new(
        config.scheduler_base_url.clone(),
        config.service_token.clone(),
    )
    .context("failed to build scheduler client")?;

    let state = AppState::new(config.clone(), Arc::new(identity));
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("zukka-portal listening on {}", config.bind);
    axum::serve(listener, app).await?;
    Ok(())
}

/// `--healthcheck` implementation: the shared database is readable.
async fn healthcheck() -> anyhow::Result<()> {
    let db_path = std::env::var("ZUKKA_DB").unwrap_or_else(|_| "/data/zukka.db".into());
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db_path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open database read-only at {db_path}"))?;
    sqlx::query("SELECT 1")
        .fetch_one(&pool)
        .await
        .context("database not readable")?;
    pool.close().await;
    Ok(())
}
