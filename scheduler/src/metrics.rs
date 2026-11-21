//! Prometheus metrics for the scheduler (Issue #44 — Monitoring, §3.8/§4.2).
//!
//! `GET /metrics` renders the Prometheus text exposition format
//! (`text/plain; version=0.0.4`, §4.2). The endpoint is deliberately open (no
//! auth): it is a scrape path, documented on `:9091` behind the Caddy bridge
//! `:12091` (§3.8) and additionally mounted on the main router at `/metrics`.
//!
//! Metrics (minimum set, §3.8):
//!
//! | Metric                            | Type    | Source                              |
//! | --------------------------------- | ------- | ----------------------------------- |
//! | `zukka_icecast_listeners`         | gauge   | reuses [`crate::api::ops`] poll     |
//! | `zukka_live_sources`              | gauge   | `store::control::live_source_count` |
//! | `zukka_source_state{layer}`       | gauge   | `store::control::state` (6 layers)  |
//! | `zukka_queue_fill_seconds{queue}` | gauge   | SQL join `queue_items` → `tracks`   |
//! | `zukka_hls_segment_age_seconds`   | gauge   | newest file in `ZUKKA_HLS_DIR`      |
//! | `zukka_music_api_errors_total`    | counter | external-authority poll failures    |
//! | `zukka_scheduler_db_locked_total` | counter | SQLite busy/locked (`error.rs`)     |
//! | `zukka_build_info{version,sha}`   | gauge=1 | `ZUKKA_VERSION` / `ZUKKA_GIT_SHA`   |
//!
//! The counters are process-local (`AtomicU64`) and therefore reset on every
//! restart — the `# HELP` text says "since process start" so `rate()` users are
//! not surprised (a restart just looks like a counter reset).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sqlx::SqlitePool;

use crate::dto::SourceLayer;

/// Content type of the Prometheus text exposition format.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// The six fixed layers (§3.2/C77), in priority order.
const LAYERS: [SourceLayer; 6] = [
    SourceLayer::Live,
    SourceLayer::Show,
    SourceLayer::Queue,
    SourceLayer::Rotation,
    SourceLayer::Relay,
    SourceLayer::Fun,
];

/// Default directory holding HLS segments (shared `hls` volume, §3.8).
const DEFAULT_HLS_DIR: &str = "/hls";

/// Counter: external-authority (Music API / stream status) poll failures,
/// since process start.
///
/// The scheduler has no direct Music-API client of its own (that lives in the
/// portal, §3.5); the external content authority it *does* poll is the Icecast
/// status document. A poll failure therefore means "the external authority was
/// not reachable" and is counted here (§3.8).
pub static MUSIC_API_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Counter: SQLite `busy`/`locked` errors observed while running a query,
/// since process start.
pub static SCHEDULER_DB_LOCKED: AtomicU64 = AtomicU64::new(0);

/// Records one external-authority poll failure (see [`MUSIC_API_ERRORS`]).
pub fn record_music_api_error() {
    MUSIC_API_ERRORS.fetch_add(1, Ordering::Relaxed);
}

/// Records one SQLite `busy`/`locked` error (see [`SCHEDULER_DB_LOCKED`]).
pub fn record_scheduler_db_locked() {
    SCHEDULER_DB_LOCKED.fetch_add(1, Ordering::Relaxed);
}

/// True when `err` is a SQLite `busy`/`locked` error.
///
/// `sqlx` surfaces these as a `DatabaseError` whose message contains
/// `database is locked` (SQLITE_BUSY) or `database table is locked`.
pub fn is_db_locked(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => {
            let message = db.message().to_ascii_lowercase();
            message.contains("database is locked") || message.contains("database table is locked")
        }
        _ => false,
    }
}

/// Escapes a Prometheus label value: `\` → `\\`, `"` → `\"`, newline → `\n`.
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

/// Appends `# HELP`/`# TYPE` for one metric.
fn header_block(out: &mut String, name: &str, help: &str, kind: &str) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} {kind}\n"));
}

/// Build version: `ZUKKA_VERSION`, else the crate version.
fn build_version() -> String {
    std::env::var("ZUKKA_VERSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// Build SCM revision: `ZUKKA_GIT_SHA`, else empty (never fabricated).
fn build_sha() -> String {
    std::env::var("ZUKKA_GIT_SHA")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

/// HLS directory (`ZUKKA_HLS_DIR`, default `/hls`).
fn hls_dir() -> String {
    std::env::var("ZUKKA_HLS_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_HLS_DIR.to_string())
}

/// Age of the newest file in `dir`, in whole seconds.
///
/// `None` when the directory is absent, empty or unreadable, or when the
/// newest mtime is in the future — the metric is then omitted (no fake value).
fn hls_newest_age_secs(dir: &str) -> Option<f64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut newest: Option<SystemTime> = None;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        newest = Some(match newest {
            Some(current) if current >= modified => current,
            _ => modified,
        });
    }
    let newest = newest?;
    let age = SystemTime::now().duration_since(newest).ok()?;
    Some(age.as_secs_f64())
}

/// Renders the full Prometheus exposition text for `pool`.
///
/// Read-only: every source is a `SELECT` or a read of the `hls` volume. A
/// failed source degrades to `0` (or omission for HLS) with a warning — a
/// scrape must never 5xx.
pub async fn render(pool: &SqlitePool) -> String {
    let mut out = String::new();

    // zukka_icecast_listeners — reuses the ops poll (§4.2/#42).
    header_block(
        &mut out,
        "zukka_icecast_listeners",
        "Total listeners reported by the Icecast status document (0 when unreachable).",
        "gauge",
    );
    let listeners = crate::api::ops::fetch_listeners().await;
    out.push_str(&format!("zukka_icecast_listeners {listeners}\n"));

    // zukka_live_sources — running live sources (live broadcasts + override).
    header_block(
        &mut out,
        "zukka_live_sources",
        "Number of running live sources (live broadcasts plus an active override).",
        "gauge",
    );
    let live_sources = match crate::store::control::live_source_count(pool).await {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("metrics: live_source_count failed: {err:#}");
            0
        }
    };
    out.push_str(&format!("zukka_live_sources {live_sources}\n"));

    // zukka_source_state{layer} — 1 for the active layer, else 0.
    header_block(
        &mut out,
        "zukka_source_state",
        "Whether a priority layer is the active output layer (1) or not (0).",
        "gauge",
    );
    let (active_layer, observed_layer) = match crate::store::control::state(pool).await {
        Ok(state) => (state.active_layer, state.observed_layer),
        Err(err) => {
            tracing::warn!("metrics: control state failed: {err:#}");
            (None, None)
        }
    };
    for layer in LAYERS {
        let value = u8::from(active_layer == Some(layer));
        out.push_str(&format!(
            "zukka_source_state{{layer=\"{}\"}} {value}\n",
            escape_label(layer.as_str())
        ));
    }

    // zukka_source_observed{layer} — #140: 1 for the Icecast-observed layer.
    // `zukka_source_state` keeps its derived/effective contract above.
    header_block(
        &mut out,
        "zukka_source_observed",
        "Whether a priority layer was observed on the Icecast output (1) or not (0).",
        "gauge",
    );
    for layer in LAYERS {
        let value = u8::from(observed_layer == Some(layer));
        out.push_str(&format!(
            "zukka_source_observed{{layer=\"{}\"}} {value}\n",
            escape_label(layer.as_str())
        ));
    }

    // zukka_queue_fill_seconds{queue} — pending playable duration per queue.
    header_block(
        &mut out,
        "zukka_queue_fill_seconds",
        "Approximate pending playable duration per queue in seconds; sums \
         tracks.duration_ms of pending track items (items without a track count 0).",
        "gauge",
    );
    match crate::store::queues::fill_seconds(pool).await {
        Ok(rows) => {
            for (queue_id, seconds) in rows {
                out.push_str(&format!(
                    "zukka_queue_fill_seconds{{queue=\"{}\"}} {seconds}\n",
                    escape_label(&queue_id)
                ));
            }
        }
        Err(err) => tracing::warn!("metrics: queue fill failed: {err:#}"),
    }

    // zukka_hls_segment_age_seconds — omitted when no segments exist (#84).
    if let Some(age) = hls_newest_age_secs(&hls_dir()) {
        header_block(
            &mut out,
            "zukka_hls_segment_age_seconds",
            "Age in seconds of the newest HLS segment file (omitted when absent).",
            "gauge",
        );
        out.push_str(&format!("zukka_hls_segment_age_seconds {age}\n"));
    }

    // Counters (process-local, reset on restart).
    header_block(
        &mut out,
        "zukka_music_api_errors_total",
        "External-authority poll failures since process start (counter resets on restart).",
        "counter",
    );
    out.push_str(&format!(
        "zukka_music_api_errors_total {}\n",
        MUSIC_API_ERRORS.load(Ordering::Relaxed)
    ));

    header_block(
        &mut out,
        "zukka_scheduler_db_locked_total",
        "SQLite busy/locked errors observed since process start (counter resets on restart).",
        "counter",
    );
    out.push_str(&format!(
        "zukka_scheduler_db_locked_total {}\n",
        SCHEDULER_DB_LOCKED.load(Ordering::Relaxed)
    ));

    // zukka_build_info{version,sha} = 1.
    header_block(
        &mut out,
        "zukka_build_info",
        "Build information; the value is always 1 (labels carry version and sha).",
        "gauge",
    );
    out.push_str(&format!(
        "zukka_build_info{{version=\"{}\",sha=\"{}\"}} 1\n",
        escape_label(&build_version()),
        escape_label(&build_sha())
    ));

    out
}

/// `GET /metrics` handler for the main router (§4.2).
pub async fn metrics(State(pool): State<SqlitePool>) -> Response {
    let body = render(&pool).await;
    (StatusCode::OK, [(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response()
}

/// `GET /healthz` handler for the dedicated metrics listener.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Router served by the dedicated metrics listener: `/metrics` + `/healthz`.
pub fn router(pool: SqlitePool) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/metrics", get(metrics))
        .route("/healthz", get(healthz))
        .with_state(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_label_escapes_backslash_quote_newline() {
        assert_eq!(escape_label(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label("a\nb"), r"a\nb");
        assert_eq!(escape_label("plain"), "plain");
    }

    #[test]
    fn is_db_locked_rejects_non_database_errors() {
        assert!(!is_db_locked(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn counters_increment_monotonically() {
        let before = MUSIC_API_ERRORS.load(Ordering::Relaxed);
        record_music_api_error();
        assert_eq!(MUSIC_API_ERRORS.load(Ordering::Relaxed), before + 1);
        let db_before = SCHEDULER_DB_LOCKED.load(Ordering::Relaxed);
        record_scheduler_db_locked();
        assert_eq!(SCHEDULER_DB_LOCKED.load(Ordering::Relaxed), db_before + 1);
    }

    #[test]
    fn hls_age_missing_dir_is_none() {
        assert!(hls_newest_age_secs("/nonexistent-zukka-hls-dir").is_none());
    }

    #[test]
    fn hls_age_empty_dir_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(hls_newest_age_secs(dir.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn hls_age_reports_newest_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("seg0.ts"), b"old").unwrap();
        let age = hls_newest_age_secs(dir.path().to_str().unwrap()).unwrap();
        assert!((0.0..5.0).contains(&age), "age {age} not near zero");
    }

    #[test]
    fn build_sha_defaults_to_empty() {
        // SAFETY: single-threaded test; no other test sets this variable.
        unsafe { std::env::remove_var("ZUKKA_GIT_SHA") };
        assert_eq!(build_sha(), "");
    }
}
