//! Announcement dispatcher (Issue #37): poll the queue and push via a
//! [`Transport`].
//!
//! The store owns the retry state machine (`due` → `mark_sent`/`mark_failed`).
//! This module only walks the due rows sequentially; a single transport error
//! never aborts the batch, and store errors are logged without stopping the
//! loop.

use std::time::Duration;

use chrono::Utc;
use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::announcement::delivery::{OutboundAnnouncement, Transport};
use crate::announcement::telegram::{self, TelegramConfig, TelegramTransport};
use crate::error::ApiResult;
use crate::store::announcements;

/// Outcome of one dispatch pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatchReport {
    pub sent: usize,
    pub failed: usize,
}

/// Sends every currently due announcement once.
///
/// `Ok(())` from the transport → `mark_sent`; a [`DeliveryError`] →
/// `mark_failed` (backoff/terminal handling stays in the store). Both counts
/// are returned for tests and logging.
pub async fn run_once(pool: &SqlitePool, transport: &dyn Transport) -> ApiResult<DispatchReport> {
    let due = announcements::due(pool, Utc::now()).await?;
    let mut report = DispatchReport::default();

    for row in due {
        let outbound = OutboundAnnouncement {
            announcement_id: row.id.clone(),
            channel: row.channel,
            template_id: row.template_id.clone(),
            payload: row.payload.clone(),
        };
        let now = Utc::now();
        match transport.send(outbound).await {
            Ok(()) => {
                report.sent += 1;
                if let Err(err) = announcements::mark_sent(pool, &row.id, now).await {
                    tracing::warn!(
                        announcement_id = %row.id,
                        "failed to mark announcement as sent: {err}"
                    );
                }
            }
            Err(err) => {
                report.failed += 1;
                if let Err(mark_err) =
                    announcements::mark_failed(pool, &row.id, &err.message, now).await
                {
                    tracing::warn!(
                        announcement_id = %row.id,
                        "failed to record announcement failure: {mark_err}"
                    );
                }
            }
        }
    }

    Ok(report)
}

/// Starts the background poll loop unless Telegram is not configured.
///
/// `None` (no token/chat) means delivery is disabled and no task is spawned.
pub fn spawn(pool: SqlitePool) -> Option<JoinHandle<()>> {
    let config = TelegramConfig::from_env()?;
    let secs = telegram::poll_secs_from_env();
    tracing::info!(
        channel = config.channel.as_str(),
        interval_secs = secs,
        "announcement delivery enabled"
    );
    let transport = TelegramTransport::new(pool.clone(), config);

    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match run_once(&pool, &transport).await {
                Ok(report) if report.sent + report.failed > 0 => {
                    tracing::info!(
                        sent = report.sent,
                        failed = report.failed,
                        "announcement dispatch pass"
                    );
                }
                Ok(_) => {}
                Err(err) => tracing::warn!("announcement dispatch pass failed: {err}"),
            }
        }
    }))
}
