//! Ops endpoints (Issue #42 — Deploy-Gate, §2.9/§4.2).
//!
//! `GET /internal/ops/audience` is the single source the PROD deploy wrapper
//! (`zukka-deploy prod --check-window`) consults before touching PROD. It is
//! mounted on the `internal` router, i.e. behind `auth::require_service_token`.
//!
//! The endpoint is deliberately **fail-open** on the listener count: an
//! unreachable or malformed Icecast `status-json.xsl` yields `0` listeners plus
//! a `tracing::warn!`, never a 5xx (the gate must not be disabled by a dead
//! Icecast). This is a documented residual risk — see `docs/deploy-prod.md`.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use sqlx::SqlitePool;

use crate::dto::AudienceDto;
use crate::error::ApiResult;
use crate::store;
use crate::time::{fmt_api_time, parse_storage_time};

/// Env var holding the Icecast status URL (#42, unprefixed — set in
/// `compose/*.yml` from `ZUKKA_ICECAST_STATUS_URL`).
pub const ICECAST_STATUS_URL_ENV: &str = "ICECAST_STATUS_URL";

/// Default Icecast status endpoint: scheduler and icecast share the `back`
/// network, where the service is reachable as `icecast:8000`.
const DEFAULT_ICECAST_STATUS_URL: &str = "http://icecast:8000/status-json.xsl";

/// Icecast poll timeout — the gate must stay fast (< 1 s).
const ICECAST_TIMEOUT: Duration = Duration::from_secs(1);

/// Configured Icecast status URL, or the in-network default.
fn status_url() -> String {
    std::env::var(ICECAST_STATUS_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ICECAST_STATUS_URL.to_string())
}

/// `GET /internal/ops/audience` — gate snapshot (§4.2, #42).
pub async fn audience(State(pool): State<SqlitePool>) -> ApiResult<Json<AudienceDto>> {
    let live_sources = store::control::live_source_count(&pool).await?;
    let next_broadcast_start = match store::broadcasts::next_scheduled_start(&pool).await? {
        Some(raw) => Some(fmt_api_time(parse_storage_time(&raw)?)),
        None => None,
    };
    let listeners = fetch_listeners().await;
    Ok(Json(AudienceDto {
        listeners,
        live_sources,
        next_broadcast_start,
    }))
}

/// Best-effort listener count; any failure logs and reports `0` (never 5xx).
///
/// Reused by the metrics endpoint (§3.8, `zukka_icecast_listeners`) so gate and
/// metric share the same Icecast source. A failure also bumps the external-
/// authority error counter (§3.8, `zukka_music_api_errors_total`).
pub(crate) async fn fetch_listeners() -> u64 {
    let url = status_url();
    match fetch_and_sum(&url).await {
        Ok(listeners) => listeners,
        Err(err) => {
            crate::metrics::record_music_api_error();
            tracing::warn!(
                url,
                "icecast status poll failed; reporting 0 listeners: {err}"
            );
            0
        }
    }
}

/// Fetches the Icecast status document and sums its source listeners.
async fn fetch_and_sum(url: &str) -> Result<u64, String> {
    let client = reqwest::Client::builder()
        .timeout(ICECAST_TIMEOUT)
        .build()
        .map_err(|err| format!("building HTTP client failed: {err}"))?;
    let body = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("GET {url} failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("GET {url} returned an error: {err}"))?
        .text()
        .await
        .map_err(|err| format!("reading {url} failed: {err}"))?;
    sum_listeners(&body).ok_or_else(|| format!("malformed JSON from {url}"))
}

/// Sums the `listeners` of every source in an Icecast `status-json.xsl` body.
///
/// `icestats.source` may be a single object or an array. Missing `listeners`
/// fields or non-numeric values count as `0`; a missing `source` (no streams)
/// is `0`. Returns `None` only when the JSON itself is unparseable.
pub fn sum_listeners(body: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let source = match value.get("icestats").and_then(|s| s.get("source")) {
        Some(source) => source,
        // No `icestats.source` at all ⇒ no streams ⇒ 0 listeners.
        None => return Some(0),
    };
    let sources: Vec<&serde_json::Value> = match source {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![source],
        // `source` present but neither object nor array (e.g. `null`) ⇒ 0.
        _ => return Some(0),
    };
    let total = sources
        .iter()
        .filter_map(|source| source.get("listeners"))
        .filter_map(serde_json::Value::as_u64)
        .sum();
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::sum_listeners;

    #[test]
    fn single_source_object() {
        let body = r#"{"icestats":{"source":{"listeners":7,"server_name":"zukka"}}}"#;
        assert_eq!(sum_listeners(body), Some(7));
    }

    #[test]
    fn source_array_sums() {
        let body = r#"{"icestats":{"source":[{"listeners":3},{"listeners":4}]}}"#;
        assert_eq!(sum_listeners(body), Some(7));
    }

    #[test]
    fn no_source_is_zero() {
        let body = r#"{"icestats":{"admin":"x"}}"#;
        assert_eq!(sum_listeners(body), Some(0));
    }

    #[test]
    fn missing_listeners_field_counts_zero() {
        let body = r#"{"icestats":{"source":{"server_name":"zukka"}}}"#;
        assert_eq!(sum_listeners(body), Some(0));
    }

    #[test]
    fn null_source_is_zero() {
        let body = r#"{"icestats":{"source":null}}"#;
        assert_eq!(sum_listeners(body), Some(0));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(sum_listeners("not json"), None);
        assert_eq!(sum_listeners("<html>"), None);
    }
}
