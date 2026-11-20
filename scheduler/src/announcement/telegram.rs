//! Telegram bot transport (Issue #37).
//!
//! Implements the [`Transport`] seam from [`super::delivery`] against the
//! Telegram Bot API. The queue/retry state machine stays in
//! `store::announcements`; this module only renders text and performs one
//! HTTP request per announcement.
//!
//! ## Hermeticity
//!
//! [`TelegramConfig::from_env`] returns `None` when the bot token **or** the
//! main chat id is unset/blank. The dispatcher then stays a no-op, so unit and
//! integration tests never perform real network calls.
//!
//! ## Secrets
//!
//! The bot token is part of the request URL. Unlike `liquidsoap.rs` (which
//! embeds the endpoint URL in error strings) this module never propagates a
//! URL or the token: error messages carry only the HTTP status and Telegram's
//! `description`, with any accidental token occurrence redacted.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::announcement::delivery::{DeliveryError, OutboundAnnouncement, Transport};
use crate::announcement::render;
use crate::dto::AnnouncementChannel;
use crate::store;

/// Environment variable holding the bot token.
pub const BOT_TOKEN_ENV: &str = "ZUKKA_TELEGRAM_BOT_TOKEN";
/// Environment variable holding the main channel chat id.
pub const CHAT_ID_ENV: &str = "ZUKKA_TELEGRAM_CHAT_ID";
/// DEV-only test channel chat id (ignored outside `ZUKKA_ENV=dev`).
pub const CHAT_ID_TEST_ENV: &str = "ZUKKA_TELEGRAM_CHAT_ID_TEST";
/// Base URL of the Bot API (mock servers in tests).
pub const API_BASE_ENV: &str = "ZUKKA_TELEGRAM_API_BASE";
/// Environment selector (`dev`/`prod`/…).
pub const ENV_ENV: &str = "ZUKKA_ENV";
/// Environment variable for the poll interval (seconds).
pub const POLL_SECS_ENV: &str = "ZUKKA_ANNOUNCEMENT_POLL_SECS";

/// Default Bot API base.
pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";
/// Default poll interval in seconds.
pub const DEFAULT_POLL_SECS: u64 = 15;
/// Telegram hard limit for one message.
pub const MAX_TEXT_CHARS: usize = 4096;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Which chat an announcement is routed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Main,
    Test,
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Main => "main",
            ChannelKind::Test => "test",
        }
    }
}

/// Resolved Telegram configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramConfig {
    pub token: String,
    pub chat_id: String,
    pub api_base: String,
    pub channel: ChannelKind,
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Pure channel selection: in `dev` a set test chat wins, every other
/// environment ignores it. `None` when the main chat is unset.
pub fn resolve_chat(env: Option<&str>, main: Option<&str>, test: Option<&str>) -> Option<String> {
    let main = trimmed(main)?;
    if env.map(str::trim) == Some("dev") {
        if let Some(test) = trimmed(test) {
            return Some(test.to_string());
        }
    }
    Some(main.to_string())
}

/// Pure config construction (no env access — easy to unit-test).
pub fn config_from(
    token: Option<&str>,
    main_chat: Option<&str>,
    test_chat: Option<&str>,
    env: Option<&str>,
    api_base: Option<&str>,
) -> Option<TelegramConfig> {
    let token = trimmed(token)?.to_string();
    let chat_id = resolve_chat(env, main_chat, test_chat)?;
    let channel = if env.map(str::trim) == Some("dev") && trimmed(test_chat).is_some() {
        ChannelKind::Test
    } else {
        ChannelKind::Main
    };
    let api_base = trimmed(api_base)
        .unwrap_or(DEFAULT_API_BASE)
        .trim_end_matches('/')
        .to_string();
    Some(TelegramConfig {
        token,
        chat_id,
        api_base,
        channel,
    })
}

impl TelegramConfig {
    /// Reads the process environment; `None` disables delivery.
    pub fn from_env() -> Option<TelegramConfig> {
        let token = std::env::var(BOT_TOKEN_ENV).ok();
        let main_chat = std::env::var(CHAT_ID_ENV).ok();
        let test_chat = std::env::var(CHAT_ID_TEST_ENV).ok();
        let env = std::env::var(ENV_ENV).ok();
        let api_base = std::env::var(API_BASE_ENV).ok();
        config_from(
            token.as_deref(),
            main_chat.as_deref(),
            test_chat.as_deref(),
            env.as_deref(),
            api_base.as_deref(),
        )
    }
}

/// Poll interval from `ZUKKA_ANNOUNCEMENT_POLL_SECS` (default 15, min 1).
pub fn poll_secs_from_env() -> u64 {
    std::env::var(POLL_SECS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_SECS)
        .max(1)
}

/// Renders an announcement's text from its template body and payload.
///
/// A missing `template_id`, a deleted template (`ON DELETE SET NULL`) or an
/// empty result is a [`DeliveryError`] — no HTTP call happens.
pub async fn render_text(
    pool: &SqlitePool,
    msg: &OutboundAnnouncement,
) -> Result<String, DeliveryError> {
    let Some(template_id) = msg.template_id.as_deref() else {
        return Err(DeliveryError::new(
            "announcement has no template to render".to_string(),
        ));
    };
    let template = store::announcement_templates::get(pool, template_id)
        .await
        .map_err(|_| DeliveryError::new(format!("template {template_id} is not available")))?;
    let payload = render::payload_from_json(&msg.payload);
    let text = render::render(&template.body, &payload);
    if text.trim().is_empty() {
        return Err(DeliveryError::new(
            "rendered announcement text is empty".to_string(),
        ));
    }
    Ok(text)
}

/// Builds the request target and JSON body for `sendMessage`.
pub fn build_request(base: &str, token: &str, chat_id: &str, text: &str) -> (String, Value) {
    let url = format!(
        "{}/bot{}/sendMessage",
        base.trim_end_matches('/'),
        token.trim()
    );
    let body = json!({ "chat_id": chat_id, "text": text });
    (url, body)
}

/// Truncates `text` to at most `max` **characters** (never bytes), keeping
/// Unicode scalars intact.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("reqwest client")
    })
}

/// Replaces any occurrence of `token` with `<redacted>`.
fn sanitize(message: &str, token: &str) -> String {
    if token.is_empty() {
        return message.to_string();
    }
    message.replace(token, "<redacted>")
}

/// Real Telegram transport: renders a message and POSTs it to the Bot API.
#[derive(Debug, Clone)]
pub struct TelegramTransport {
    pool: SqlitePool,
    config: TelegramConfig,
}

impl TelegramTransport {
    pub fn new(pool: SqlitePool, config: TelegramConfig) -> Self {
        Self { pool, config }
    }

    fn describe_error(&self, status: reqwest::StatusCode, body: &str) -> DeliveryError {
        let description = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| body.trim().chars().take(200).collect());
        let message = if description.is_empty() {
            format!("telegram sendMessage returned HTTP {status}")
        } else {
            format!("telegram sendMessage returned HTTP {status}: {description}")
        };
        DeliveryError::new(sanitize(&message, &self.config.token))
    }

    async fn send_inner(&self, msg: OutboundAnnouncement) -> Result<(), DeliveryError> {
        if msg.channel != AnnouncementChannel::Telegram {
            return Err(DeliveryError::new(format!(
                "unsupported announcement channel '{}'",
                msg.channel.as_str()
            )));
        }

        let mut text = render_text(&self.pool, &msg).await?;
        if text.chars().count() > MAX_TEXT_CHARS {
            tracing::warn!(
                announcement_id = %msg.announcement_id,
                limit = MAX_TEXT_CHARS,
                "telegram message exceeds the length limit and was truncated"
            );
            text = truncate_chars(&text, MAX_TEXT_CHARS);
        }

        let (url, body) = build_request(
            &self.config.api_base,
            &self.config.token,
            &self.config.chat_id,
            &text,
        );
        let response = client()
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|err| {
                // Never include the URL (it carries the token) or `err`'s Display,
                // which may contain the URL.
                let reason = if err.is_timeout() {
                    "request timed out"
                } else if err.is_connect() {
                    "connection failed"
                } else {
                    "request failed"
                };
                DeliveryError::new(format!("telegram sendMessage {reason}"))
            })?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|_| DeliveryError::new("telegram sendMessage response could not be read"))?;

        if !status.is_success() {
            return Err(self.describe_error(status, &raw));
        }

        // Telegram answers 200 with `{"ok": false, "description": ...}` for
        // some failures; treat that as an error too.
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if value.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(self.describe_error(status, &raw));
            }
        }

        Ok(())
    }
}

impl Transport for TelegramTransport {
    fn send(
        &self,
        msg: OutboundAnnouncement,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), DeliveryError>> + Send>>
    {
        // The seam's boxed future is `'static`, so move owned state in.
        let pool = self.pool.clone();
        let config = self.config.clone();
        Box::pin(async move { TelegramTransport { pool, config }.send_inner(msg).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_chat_dev_uses_test_channel() {
        assert_eq!(
            resolve_chat(Some("dev"), Some("main"), Some("test")),
            Some("test".to_string())
        );
    }

    #[test]
    fn resolve_chat_prod_ignores_test_channel() {
        assert_eq!(
            resolve_chat(Some("prod"), Some("main"), Some("test")),
            Some("main".to_string())
        );
        assert_eq!(
            resolve_chat(None, Some("main"), Some("test")),
            Some("main".to_string())
        );
    }

    #[test]
    fn resolve_chat_dev_without_test_falls_back_to_main() {
        assert_eq!(
            resolve_chat(Some("dev"), Some("main"), None),
            Some("main".to_string())
        );
        assert_eq!(
            resolve_chat(Some("dev"), Some("main"), Some("   ")),
            Some("main".to_string())
        );
    }

    #[test]
    fn resolve_chat_trims_and_handles_missing_main() {
        assert_eq!(
            resolve_chat(Some("dev"), Some(" main "), Some(" test ")),
            Some("test".to_string())
        );
        assert_eq!(resolve_chat(Some("prod"), None, Some("test")), None);
        assert_eq!(resolve_chat(Some("prod"), Some("  "), Some("test")), None);
    }

    #[test]
    fn config_requires_token_and_chat() {
        assert!(config_from(None, Some("c"), None, Some("prod"), None).is_none());
        assert!(config_from(Some("t"), None, None, Some("prod"), None).is_none());
        assert!(config_from(Some("  "), Some("c"), None, Some("prod"), None).is_none());
        assert!(config_from(Some("t"), Some("  "), None, Some("prod"), None).is_none());
    }

    #[test]
    fn config_defaults_and_channel_kind() {
        let main = config_from(Some("tok"), Some("main"), None, Some("prod"), None).unwrap();
        assert_eq!(main.api_base, DEFAULT_API_BASE);
        assert_eq!(main.channel, ChannelKind::Main);
        assert_eq!(main.chat_id, "main");

        let test = config_from(
            Some("tok"),
            Some("main"),
            Some("test"),
            Some("dev"),
            Some("http://x/"),
        )
        .unwrap();
        assert_eq!(test.channel, ChannelKind::Test);
        assert_eq!(test.chat_id, "test");
        assert_eq!(test.api_base, "http://x");
    }

    #[test]
    fn build_request_shape() {
        let (url, body) = build_request("https://api.telegram.org", "123:abc", "42", "Hallo");
        assert_eq!(url, "https://api.telegram.org/bot123:abc/sendMessage");
        assert_eq!(body["chat_id"], "42");
        assert_eq!(body["text"], "Hallo");
    }

    #[test]
    fn build_request_strips_trailing_slash() {
        let (url, _) = build_request("http://127.0.0.1:9/", "t", "c", "x");
        assert_eq!(url, "http://127.0.0.1:9/bott/sendMessage");
    }

    #[test]
    fn truncate_is_unicode_safe() {
        let text = "🎙️🎶abc";
        let cut = truncate_chars(text, 3);
        assert_eq!(cut.chars().count(), 3);
        assert!(text.starts_with(&cut));
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[test]
    fn sanitize_removes_token() {
        let out = sanitize("url bot123:secret failed", "123:secret");
        assert!(!out.contains("123:secret"));
        assert!(out.contains("<redacted>"));
    }
}
