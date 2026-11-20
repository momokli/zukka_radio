//! Public Tapes API server (`zukka-tapes`).

use anyhow::Context;

use zukka_tapes::config::Config;
use zukka_tapes::db;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env();
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

    let app = zukka_tapes::api::router(pool, config.base_url.clone());

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("zukka-tapes listening on {}", config.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
