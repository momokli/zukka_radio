//! Sender seam for announcements (Issue #35) — **definition only**.
//!
//! #35 fills the queue (`announcements`) and the retry state machine. Actual
//! transport (Telegram bot, HTTP client, token) is #37. This module only
//! fixes the contract so #37 can implement it without touching the store/API.
//!
//! Deliberately no `async-trait`/`futures` dependency: a boxed `Future`
//! suffices for a single method.

use std::future::Future;
use std::pin::Pin;

use crate::dto::AnnouncementChannel;

/// A rendered, ready-to-send announcement.
///
/// #37 builds `text` via `render::render(template.body,
/// &render::payload_from_json(&payload))`; #35 only stores `template_id` and
/// `payload`.
#[derive(Debug, Clone)]
pub struct OutboundAnnouncement {
    pub announcement_id: String,
    pub channel: AnnouncementChannel,
    pub template_id: Option<String>,
    pub payload: serde_json::Value,
}

/// Transport failure → `mark_failed(error)`. Retry policy lives in the store,
/// never in the transport.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DeliveryError {
    pub message: String,
}

impl DeliveryError {
    /// Convenience constructor.
    pub fn new(message: impl Into<String>) -> Self {
        DeliveryError {
            message: message.into(),
        }
    }
}

/// The seam #37 implements (Telegram). #35 never calls it.
pub trait Transport: Send + Sync {
    fn send(
        &self,
        msg: OutboundAnnouncement,
    ) -> Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test-only transport proving the trait is implementable without extra
    /// dependencies. #37 replaces it with the real Telegram transport.
    struct NoopTransport;

    impl Transport for NoopTransport {
        fn send(
            &self,
            _msg: OutboundAnnouncement,
        ) -> Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn noop_transport_implements_trait() {
        let outbound = OutboundAnnouncement {
            announcement_id: "a1".into(),
            channel: AnnouncementChannel::Telegram,
            template_id: Some("t1".into()),
            payload: json!({ "show_name": "Morning" }),
        };
        let transport: Box<dyn Transport> = Box::new(NoopTransport);
        transport.send(outbound).await.expect("noop send");
    }

    #[test]
    fn delivery_error_displays_message() {
        let err = DeliveryError::new("boom");
        assert_eq!(err.to_string(), "boom");
    }
}
