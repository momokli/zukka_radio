use anyhow::Context;

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

    // C43: after a restart, replay the persisted live override so Liquidsoap
    // and the control panel do not drift back to FCFS silently.
    if let Err(err) = zukka_scheduler::store::control::reassert_live(&pool).await {
        tracing::warn!("failed to re-assert live override: {err}");
    }

    // Boot guarantee: the rotation file exists as soon as the process starts,
    // so Liquidsoap's `watch` reload always sees a valid playlist (§3c).
    if let Err(err) = zukka_scheduler::store::rotation::sync(&pool).await {
        tracing::warn!("failed to sync rotation.m3u at boot: {err}");
    }

    // US-10: no `running` upload job may survive a restart without a timeout.
    let timeout = zukka_scheduler::recovery::timeout_secs_from_env();
    match zukka_scheduler::recovery::recover_stale_uploads(
        &pool,
        timeout,
        zukka_scheduler::recovery::DEFAULT_MAX_ATTEMPTS,
    )
    .await
    {
        Ok(report) if report.requeued + report.failed > 0 => {
            tracing::warn!(
                "recovered stale upload jobs: {} requeued, {} failed (timeout {timeout}s)",
                report.requeued,
                report.failed
            );
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("upload recovery failed: {err}"),
    }

    let app = zukka_scheduler::api::router(pool);

    let addr = std::env::var("ZUKKA_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("zukka-scheduler listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
