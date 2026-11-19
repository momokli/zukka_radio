//! Auth: session cookie, CSRF, middleware, handlers.
//!
//! The portal holds no authoritative session state (portal.md D3): the cookie
//! carries the raw token, and every request re-reads the session from the
//! scheduler via `/internal/sessions/current`.

pub mod csrf;
pub mod handlers;
pub mod middleware;
pub mod session;

use std::collections::BTreeMap;

/// Per-request authentication context, built by [`middleware::resolve_session`].
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub session_id: String,
    pub caster_id: String,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub permissions: BTreeMap<String, bool>,
    pub csrf_token: String,
}

impl AuthContext {
    /// `true` when the caster holds `permission` (§6.10.1).
    pub fn has(&self, permission: &str) -> bool {
        self.permissions.get(permission).copied().unwrap_or(false)
    }
}
