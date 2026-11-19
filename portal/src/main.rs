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
