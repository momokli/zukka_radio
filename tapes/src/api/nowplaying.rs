//! `GET /api/v1/public/nowplaying` — auth-free now-playing projection
//! (Issue #107, §4.5).
//!
//! tapes polls the internal scheduler endpoint `GET {scheduler_url}/api/v1/now`
//! (service-token) and projects it to the small public shape
//! `{title, artist, show, caster, live, started_at, server_time}`. `show` /
//! `caster` are plain names, never ids or internal fields.
//!
//! **Degradation is the contract** (`docs/tapes.md` §1): a scheduler that is
//! down, slow, non-2xx or answers garbage must never make this public endpoint
//! fail. It always answers `200 no-store` — on any failure `live = false` and
//! every other field is `null`.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::AppState;
use crate::time::fmt_utc_z;

/// Upper bound for the scheduler call (§4.5, Issue #107): ≤ 2 s.
const SCHEDULER_TIMEOUT: Duration = Duration::from_secs(2);

/// Public projection returned to the player (all fields public-safe).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NowPlayingDto {
    pub title: Option<String>,
    pub artist: Option<String>,
    /// `show.name` of the running broadcast, or `null`.
    pub show: Option<String>,
    /// `caster.name` (`display_name`) of the running broadcast, or `null`.
    pub caster: Option<String>,
    pub live: bool,
    pub started_at: Option<String>,
    /// Response time, RFC-3339 UTC (`…Z`).
    pub server_time: String,
}

impl NowPlayingDto {
    /// The degraded body: `live = false`, everything else `null`.
    fn degraded(server_time: String) -> Self {
        Self {
            title: None,
            artist: None,
            show: None,
            caster: None,
            live: false,
            started_at: None,
            server_time,
        }
    }
}

/// The scheduler's `NowPlayingDto` subset tapes consumes.
///
/// Deliberately minimal: only the fields the public projection needs. serde
/// ignores the rest (`layer`, `source`) — tapes never re-exposes them.
#[derive(Debug, Deserialize)]
pub struct SchedulerNow {
    title: Option<String>,
    artist: Option<String>,
    show: Option<SchedulerName>,
    caster: Option<SchedulerName>,
    started_at: Option<String>,
    #[serde(default)]
    live: bool,
}

/// `{id, name}` — tapes keeps only `name`.
#[derive(Debug, Deserialize)]
pub struct SchedulerName {
    #[allow(dead_code)]
    id: String,
    name: String,
}

/// Why the scheduler call could not be projected (logged, never surfaced).
#[derive(Debug)]
pub enum SchedulerError {
    /// Transport error (connect/read/timeout).
    Request(String),
    /// A non-2xx status.
    Status(u16),
    /// A 2xx body that was not valid JSON.
    Json(String),
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchedulerError::Request(msg) => write!(f, "scheduler request failed: {msg}"),
            SchedulerError::Status(status) => write!(f, "scheduler returned status {status}"),
            SchedulerError::Json(msg) => write!(f, "invalid scheduler json: {msg}"),
        }
    }
}

/// Shared client + credentials for the scheduler `/now` call.
#[derive(Clone)]
pub struct NowPlayingClient {
    /// Scheduler base URL (no trailing slash).
    pub url: String,
    /// `SCHEDULER_SERVICE_TOKEN` bearer value.
    pub token: String,
    /// Shared `reqwest` client with the ≤ 2 s timeout baked in.
    pub http: reqwest::Client,
}

impl NowPlayingClient {
    /// Builds the client with a bounded timeout (never hangs a public request).
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(SCHEDULER_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            url: url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        }
    }

    fn now_url(&self) -> String {
        format!("{}/api/v1/now", self.url)
    }

    /// Fetches and classifies the scheduler `/now` response.
    pub async fn fetch(&self) -> Result<SchedulerNow, SchedulerError> {
        let response = self
            .http
            .get(self.now_url())
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|err| SchedulerError::Request(err.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|err| SchedulerError::Request(err.to_string()))?;
        classify(status, &body)
    }
}

/// Maps an HTTP status + body onto a parsed snapshot or a typed error.
fn classify(status: u16, body: &str) -> Result<SchedulerNow, SchedulerError> {
    if !(200..300).contains(&status) {
        return Err(SchedulerError::Status(status));
    }
    serde_json::from_str(body).map_err(|err| SchedulerError::Json(err.to_string()))
}

/// Projects a fetch outcome into the public DTO, always succeeding.
fn project(result: Result<SchedulerNow, SchedulerError>, now: DateTime<Utc>) -> NowPlayingDto {
    let server_time = fmt_utc_z(now);
    match result {
        Ok(snapshot) => NowPlayingDto {
            title: non_empty(snapshot.title),
            artist: non_empty(snapshot.artist),
            show: snapshot.show.and_then(|show| non_empty(Some(show.name))),
            caster: snapshot
                .caster
                .and_then(|caster| non_empty(Some(caster.name))),
            live: snapshot.live,
            started_at: snapshot.started_at,
            server_time,
        },
        Err(err) => {
            tracing::warn!("nowplaying degraded to live=false: {err}");
            NowPlayingDto::degraded(server_time)
        }
    }
}

/// `None` for absent/blank strings (public JSON never carries `""`).
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// Stamps `Cache-Control: no-store` on the response (success *and* degraded).
fn respond(dto: NowPlayingDto) -> Response {
    let mut response = (StatusCode::OK, Json(dto)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /api/v1/public/nowplaying` — always `200`, this endpoint never fails.
pub async fn get_nowplaying(State(state): State<AppState>) -> Response {
    let fetched = state.nowplaying.fetch().await;
    respond(project(fetched, Utc::now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap()
    }

    fn snapshot() -> SchedulerNow {
        SchedulerNow {
            title: Some("A Song".to_string()),
            artist: Some("An Artist".to_string()),
            show: Some(SchedulerName {
                id: "s1".to_string(),
                name: "Test Show".to_string(),
            }),
            caster: Some(SchedulerName {
                id: "c1".to_string(),
                name: "DJ Test".to_string(),
            }),
            started_at: Some("2026-11-18T11:55:00.000Z".to_string()),
            live: true,
        }
    }

    #[test]
    fn success_projects_names_and_server_time() {
        let dto = project(Ok(snapshot()), at());
        assert_eq!(dto.title.as_deref(), Some("A Song"));
        assert_eq!(dto.artist.as_deref(), Some("An Artist"));
        assert_eq!(dto.show.as_deref(), Some("Test Show"));
        assert_eq!(dto.caster.as_deref(), Some("DJ Test"));
        assert!(dto.live);
        assert_eq!(dto.started_at.as_deref(), Some("2026-11-18T11:55:00.000Z"));
        assert_eq!(dto.server_time, "2026-11-18T12:00:00Z");
    }

    #[test]
    fn degradation_covers_every_error_variant() {
        for err in [
            SchedulerError::Request("timeout".to_string()),
            SchedulerError::Status(500),
            SchedulerError::Json("bad json".to_string()),
        ] {
            let dto = project(Err(err), at());
            assert_eq!(
                dto,
                NowPlayingDto::degraded("2026-11-18T12:00:00Z".to_string())
            );
        }
    }

    #[test]
    fn blank_strings_become_null() {
        let mut snap = snapshot();
        snap.title = Some("  ".to_string());
        snap.artist = Some(String::new());
        let dto = project(Ok(snap), at());
        assert_eq!(dto.title, None);
        assert_eq!(dto.artist, None);
    }

    #[test]
    fn classify_maps_status_and_json() {
        // 2xx + valid body → parsed.
        let ok = classify(200, r#"{"live":true,"title":"x"}"#).expect("valid");
        assert!(ok.live);
        assert_eq!(ok.title.as_deref(), Some("x"));
        // non-2xx → status error, regardless of the body.
        assert!(matches!(
            classify(500, "{}"),
            Err(SchedulerError::Status(500))
        ));
        assert!(matches!(
            classify(401, r#"{"error":{}}"#),
            Err(SchedulerError::Status(401))
        ));
        // 2xx + garbage → json error.
        assert!(matches!(
            classify(200, "not json"),
            Err(SchedulerError::Json(_))
        ));
    }

    #[test]
    fn respond_is_200_and_no_store() {
        let response = respond(NowPlayingDto::degraded("2026-11-18T12:00:00Z".to_string()));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}
