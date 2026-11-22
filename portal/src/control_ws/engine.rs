//! Engine client for the control bridge (Issue #148, §4.1/§4.6).
//!
//! The bridge talks **directly** to the Liquidsoap command server (telnet over
//! HTTP: `POST <url>` with the command as `text/plain` body), mirroring
//! `scheduler/src/liquidsoap.rs`. A trait keeps the bridge hermetic in tests:
//! when `ZUKKA_LIQUIDSOAP_CONTROL_URL` is unset the production client reports
//! `enabled() == false` and every command fails with `engine_unavailable`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use crate::control_ws::protocol::{WsErrorCode, parse_engine_error};

/// Environment variable holding the full Liquidsoap control endpoint URL
/// (same name/semantics as `scheduler/src/liquidsoap.rs`).
pub const CONTROL_URL_ENV: &str = "ZUKKA_LIQUIDSOAP_CONTROL_URL";

/// `mixer.meters` — read command for the 30 Hz meter sampler (§4.5).
pub const MIXER_METERS: &str = "mixer.meters";

/// Engine failure, mapping onto the §6.4 contract codes.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    /// No endpoint configured or a transport/HTTP failure → `engine_unavailable`.
    Unavailable(String),
    /// The engine answered `ERROR <code>: …` → the mapped contract code.
    Command { code: WsErrorCode, message: String },
}

impl EngineError {
    /// The §6.4 code this error surfaces as.
    pub fn code(&self) -> WsErrorCode {
        match self {
            EngineError::Unavailable(_) => WsErrorCode::EngineUnavailable,
            EngineError::Command { code, .. } => *code,
        }
    }

    /// A human-readable message for `error.message`.
    pub fn message(&self) -> String {
        match self {
            EngineError::Unavailable(message) => message.clone(),
            EngineError::Command { message, .. } => message.clone(),
        }
    }
}

/// Outbound engine operations the bridge needs.
#[async_trait]
pub trait EngineClient: Send + Sync {
    /// Sends one command, returning the raw response body on success.
    async fn command(&self, command: &str) -> Result<String, EngineError>;

    /// Reads `mixer.meters` (JSON, §4.5).
    async fn meters(&self) -> Result<String, EngineError>;

    /// `false` when no endpoint is configured (hermetic/disabled).
    fn enabled(&self) -> bool;
}

/// Production client: `POST <url>` with a `text/plain` body, 2 s timeout.
pub struct LiquidsoapEngineClient {
    client: reqwest::Client,
    url: Option<String>,
}

impl LiquidsoapEngineClient {
    /// Builds a client against `url` (`None`/empty ⇒ disabled).
    pub fn new(url: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("reqwest client");
        let url = url
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self { client, url }
    }

    /// Sends one command, returning the response body (HTTP-level errors are
    /// `Unavailable`).
    async fn send(&self, command: &str) -> Result<String, EngineError> {
        let Some(url) = &self.url else {
            return Err(EngineError::Unavailable(
                "liquidsoap control url not configured".into(),
            ));
        };
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(command.to_string())
            .send()
            .await
            .map_err(|err| EngineError::Unavailable(format!("POST {url} failed: {err}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| EngineError::Unavailable(format!("reading {url} failed: {err}")))?;
        if !status.is_success() {
            return Err(EngineError::Unavailable(format!(
                "POST {url} returned {status}: {}",
                body.trim()
            )));
        }
        Ok(body)
    }
}

#[async_trait]
impl EngineClient for LiquidsoapEngineClient {
    async fn command(&self, command: &str) -> Result<String, EngineError> {
        if !self.enabled() {
            return Err(EngineError::Unavailable(
                "liquidsoap control url not configured".into(),
            ));
        }
        let body = self.send(command).await?;
        if let Some(code) = parse_engine_error(&body) {
            return Err(EngineError::Command {
                code,
                message: body.trim().to_string(),
            });
        }
        Ok(body)
    }

    async fn meters(&self) -> Result<String, EngineError> {
        if !self.enabled() {
            return Err(EngineError::Unavailable(
                "liquidsoap control url not configured".into(),
            ));
        }
        self.send(MIXER_METERS).await
    }

    fn enabled(&self) -> bool {
        self.url.is_some()
    }
}

/// In-memory [`EngineClient`] for tests: records the command log, serves a
/// settable meters body, and can simulate `engine_unavailable`/engine errors.
pub struct FakeEngineClient {
    enabled: AtomicBool,
    commands: Mutex<Vec<String>>,
    meters: Mutex<String>,
    command_error: Mutex<Option<EngineError>>,
    meters_error: Mutex<Option<EngineError>>,
}

impl Default for FakeEngineClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEngineClient {
    /// A fresh, enabled fake with an empty meter array.
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            commands: Mutex::new(Vec::new()),
            meters: Mutex::new("[]".to_string()),
            command_error: Mutex::new(None),
            meters_error: Mutex::new(None),
        }
    }

    /// A disabled fake (`engine_unavailable`).
    pub fn disabled() -> Self {
        let fake = Self::new();
        fake.set_enabled(false);
        fake
    }

    /// Toggles `enabled()`.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// Sets the JSON body returned by [`EngineClient::meters`].
    pub fn set_meters(&self, json: &str) {
        *self.meters.lock().unwrap() = json.to_string();
    }

    /// Makes every subsequent command fail with `error`.
    pub fn fail_commands(&self, error: EngineError) {
        *self.command_error.lock().unwrap() = Some(error);
    }

    /// Makes every subsequent `meters` read fail with `error`.
    pub fn fail_meters(&self, error: EngineError) {
        *self.meters_error.lock().unwrap() = Some(error);
    }

    /// The recorded command log, in arrival order.
    pub fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}

#[async_trait]
impl EngineClient for FakeEngineClient {
    async fn command(&self, command: &str) -> Result<String, EngineError> {
        if !self.enabled() {
            return Err(EngineError::Unavailable("engine disabled".into()));
        }
        self.commands.lock().unwrap().push(command.to_string());
        if let Some(error) = self.command_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok("OK".to_string())
    }

    async fn meters(&self) -> Result<String, EngineError> {
        if !self.enabled() {
            return Err(EngineError::Unavailable("engine disabled".into()));
        }
        if let Some(error) = self.meters_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.meters.lock().unwrap().clone())
    }

    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_client_is_engine_unavailable() {
        let client = LiquidsoapEngineClient::new(None);
        assert!(!client.enabled());
        let err = client.command("mixer.meters").await.unwrap_err();
        assert_eq!(err.code(), WsErrorCode::EngineUnavailable);
        let err = client.meters().await.unwrap_err();
        assert_eq!(err.code(), WsErrorCode::EngineUnavailable);
    }

    #[tokio::test]
    async fn fake_records_commands_and_reports_meters() {
        let fake = FakeEngineClient::new();
        assert!(fake.enabled());
        fake.set_meters(r#"[{"id":"live","rms_l":-1.0,"rms_r":-1.0,"peak":-1.0}]"#);
        assert_eq!(fake.command("mixer.set live -6").await.unwrap(), "OK");
        assert_eq!(fake.commands(), vec!["mixer.set live -6"]);
        assert!(fake.meters().await.unwrap().contains("live"));
    }

    #[tokio::test]
    async fn fake_failure_switch_maps_code() {
        let fake = FakeEngineClient::new();
        fake.fail_commands(EngineError::Command {
            code: WsErrorCode::ChannelNotFound,
            message: "ERROR unknown_channel: x".into(),
        });
        let err = fake.command("mixer.mute nope true").await.unwrap_err();
        assert_eq!(err.code(), WsErrorCode::ChannelNotFound);

        fake.fail_meters(EngineError::Unavailable("down".into()));
        assert_eq!(
            fake.meters().await.unwrap_err().code(),
            WsErrorCode::EngineUnavailable
        );
    }
}
