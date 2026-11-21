use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let db_path = std::env::var("ZUKKA_DB")
        .unwrap_or_else(|_| zukka_scheduler::db::DEFAULT_DB_PATH.to_string());

    // Distroless container healthcheck (§3.6): a shell-free probe. It opens the
    // database (running migrations, which proves the writer path works) and
    // verifies integrity. Exit 0 = ready, != 0 = fail.
    if std::env::args().any(|arg| arg == "--healthcheck") {
        if let Err(err) = healthcheck(&db_path).await {
            eprintln!("healthcheck failed: {err:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let pool = zukka_scheduler::db::connect(&db_path)
        .await
        .with_context(|| format!("failed to initialize database at {db_path}"))?;
    tracing::info!("database ready at {db_path}");

    // Issue #101: create the first admin from env when `casters` is empty.
    // Fail-open on purpose: a broken seed must never stop a running stream.
    match zukka_scheduler::store::auth::bootstrap_admin(&pool).await {
        Ok(outcome) => tracing::info!(?outcome, "bootstrap admin check"),
        Err(err) => tracing::warn!("bootstrap admin failed: {err:#}"),
    }

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

    let app = zukka_scheduler::api::router(pool.clone());

    // #37: announcement dispatch loop. The task is disabled (`None`) when
    // Telegram credentials are absent and ends with the process.
    if zukka_scheduler::announcement::dispatch::spawn(pool.clone()).is_none() {
        tracing::info!("announcement delivery disabled (telegram not configured)");
    }

    let addr = std::env::var("ZUKKA_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    // #44: dedicated metrics listener (§3.8). A bind failure is logged only —
    // it must never take down the control/API server.
    spawn_metrics_listener(pool.clone());

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("zukka-scheduler listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Spawns the dedicated metrics server on `ZUKKA_METRICS_BIND` (default
/// `0.0.0.0:9091`, §3.8), serving `/metrics` + `/healthz`.
fn spawn_metrics_listener(pool: sqlx::SqlitePool) {
    let addr = std::env::var("ZUKKA_METRICS_BIND").unwrap_or_else(|_| "0.0.0.0:9091".to_string());
    let app = zukka_scheduler::metrics::router(pool);
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                tracing::info!("zukka-scheduler metrics listening on {addr}");
                if let Err(err) = axum::serve(listener, app).await {
                    tracing::warn!("metrics server stopped: {err}");
                }
            }
            Err(err) => tracing::warn!("failed to bind metrics listener on {addr}: {err}"),
        }
    });
}

/// `--healthcheck` implementation: DB reachable, writable and intact.
async fn healthcheck(db_path: &str) -> anyhow::Result<()> {
    let pool = zukka_scheduler::db::connect(db_path)
        .await
        .with_context(|| format!("failed to initialize database at {db_path}"))?;
    let result: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&pool)
        .await
        .context("PRAGMA quick_check failed")?;
    anyhow::ensure!(result == "ok", "database integrity check failed: {result}");
    pool.close().await;
    Ok(())
}
