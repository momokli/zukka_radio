//! Outbound control channel: scheduler → Liquidsoap (issue #32).
//!
//! The Liquidsoap command server is started by `server.harbor` on port 8010
//! (internal `back` network only, §2 D14). It speaks "telnet over HTTP": a
//! command is sent as the **body** of a `POST` to `/control`, and the response
//! body is the command's return value. This was verified against the pinned
//! `savonet/liquidsoap:v2.4.5` image:
//!
//! ```text
//! POST http://liquidsoap:8010/control
//! live.set /live/slot-03
//! ```
//!
//! Configuration is explicit: when `ZUKKA_LIQUIDSOAP_CONTROL_URL` is unset the
//! push is skipped, which keeps unit/integration tests hermetic. The deployed
//! stacks set it in `env/*.env` (`http://liquidsoap:8010/control`).

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

/// Environment variable holding the full control endpoint URL.
pub const CONTROL_URL_ENV: &str = "ZUKKA_LIQUIDSOAP_CONTROL_URL";

/// `live.set <mount>` — force this caster mount on air (§4.4).
pub fn live_set(mount: &str) -> String {
    format!("live.set {}", mount.trim())
}

/// `live.clear` — drop the override and return to FCFS (§4.4).
pub const LIVE_CLEAR: &str = "live.clear";

/// `live.status` — JSON with `live_order`, `override`, `current` (§4.4).
pub const LIVE_STATUS: &str = "live.status";

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("reqwest client")
    })
}

fn configured_url() -> Option<String> {
    std::env::var(CONTROL_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Sends one command to `url`, returning the response body.
pub async fn send_to(url: &str, command: &str) -> Result<String, String> {
    let response = client()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body(command.to_string())
        .send()
        .await
        .map_err(|err| format!("POST {url} failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| format!("reading response from {url} failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("POST {url} returned {status}: {}", body.trim()));
    }
    Ok(body)
}

/// Best-effort push through the configured endpoint.
///
/// `Ok(None)` means no endpoint is configured (push disabled). Every caller in
/// the control store persists state first, so a failed push never loses the
/// operator's decision — it is re-asserted on the next scheduler start.
pub async fn push(command: &str) -> Result<Option<String>, String> {
    match configured_url() {
        Some(url) => send_to(&url, command).await.map(Some),
        None => Ok(None),
    }
}

/// Reads back the current Liquidsoap live selection (`live.status`).
pub async fn status() -> Result<Option<String>, String> {
    push(LIVE_STATUS).await
}
