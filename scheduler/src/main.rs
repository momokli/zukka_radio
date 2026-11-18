use anyhow::Context;
use axum::{Router, routing::get};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let db_path = std::env::var("ZUKKA_DB")
        .unwrap_or_else(|_| zukka_scheduler::db::DEFAULT_DB_PATH.to_string());
    let pool = zukka_scheduler::db::connect(&db_path)
        .await
        .with_context(|| format!("failed to initialize database at {db_path}"))?;
    tracing::info!("database ready at {db_path}");

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .with_state(pool);

    let addr = std::env::var("ZUKKA_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("zukka-scheduler listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
