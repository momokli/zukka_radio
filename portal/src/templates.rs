//! Askama template structs (one per page).

use askama::Template;

/// Login page (`GET /login`).
#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    pub csrf_token: String,
    pub username: String,
    pub error: Option<String>,
}

/// Dashboard (`GET /`).
#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub csrf_token: String,
    pub display_name: String,
    pub role: String,
    pub caster_id: String,
}

/// Account page (`GET /account`).
#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountPage {
    pub csrf_token: String,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub harbor_mount: Option<String>,
    pub stream_username: Option<String>,
    /// Set exactly once, right after a generated stream password.
    pub stream_password: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// Standalone error page (no session, no layout).
#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub status: u16,
    pub message: String,
}
