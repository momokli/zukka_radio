//! Askama template structs (one per page).

use askama::Template;

use crate::api_client::UploadJob;

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

/// Library page (`GET /library`): recent jobs + upload form.
#[derive(Template)]
#[template(path = "library.html")]
pub struct LibraryPage {
    pub csrf_token: String,
    pub jobs: Vec<UploadJob>,
}

/// Fragment returned by `POST /library/upload`: the freshly created jobs.
#[derive(Template)]
#[template(path = "library_upload.html")]
pub struct LibraryUploadPage {
    pub csrf_token: String,
    pub jobs: Vec<UploadJob>,
}

/// Single job fragment (`GET /library/upload/{id}`), the polling target.
#[derive(Template)]
#[template(path = "partials/upload_job.html")]
pub struct UploadJobFragment {
    pub csrf_token: String,
    pub job: UploadJob,
}

/// Inline SVG rendering of a track waveform (D8, `partials/waveform.html`).
/// `peaks` is the interleaved 8-bit `(min, max)` pair sequence (`2 * bars`).
#[derive(Template)]
#[template(path = "partials/waveform.html")]
pub struct WaveformSvg {
    pub bars: i64,
    pub peaks: Vec<u8>,
}
