use axum::{routing::get, Router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app = Router::new().route("/healthz", get(|| async { "ok" }));

    let addr = std::env::var("ZUKKA_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("zukka-scheduler listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
