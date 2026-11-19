//! Typed client for the scheduler's identity authority (`/internal/*`, §4.2).
//!
//! The portal is a thin UI: every identity operation is a call to the
//! scheduler, authenticated with `Authorization: Bearer $SCHEDULER_SERVICE_TOKEN`.
//! The `IdentityApi` trait keeps the handlers testable with a fake backend.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Caster as returned by the scheduler (`{caster}` / session validation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasterDto {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub role: String,
    pub is_active: bool,
    pub harbor_mount: Option<String>,
    pub stream_username: Option<String>,
    #[serde(default)]
    pub permissions: BTreeMap<String, bool>,
}

/// Result of `POST /internal/sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedSessionDto {
    pub id: String,
    pub token: String,
    pub csrf_token: String,
    pub idle_expires_at: String,
    pub absolute_expires_at: String,
}

/// Result of `GET /internal/sessions/current`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionValidationDto {
    pub session_id: String,
    pub caster: CasterDto,
    pub csrf_token: String,
    pub idle_expires_at: String,
    pub absolute_expires_at: String,
}

/// Identity operation failure.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Bad credentials (scheduler `401`).
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Field-level rejection (scheduler `422`).
    #[error("{message}")]
    Validation { field: String, message: String },
    /// Unknown resource (scheduler `404`).
    #[error("not found")]
    NotFound,
    /// Transport/parse failure or unexpected status.
    #[error("upstream error: {0}")]
    Upstream(String),
}

/// Identity operations the portal needs. Implemented by [`HttpIdentityApi`] in
/// production and by a fake in tests.
#[async_trait]
pub trait IdentityApi: Send + Sync {
    /// Verifies credentials; `InvalidCredentials` on `401`.
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError>;

    /// Creates a session for a caster.
    async fn create_session(
        &self,
        caster_id: &str,
        ip_address: Option<&str>,
        user_agent: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError>;

    /// Validates a raw session token; `Ok(None)` when the session is invalid.
    async fn validate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError>;

    /// Revokes a session (logout).
    async fn revoke_session(&self, session_id: &str) -> Result<(), IdentityError>;

    /// Fetches a caster by id.
    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError>;

    /// Sets the login password, keeping `keep_session_id` alive.
    async fn change_password(
        &self,
        caster_id: &str,
        password: &str,
        keep_session_id: Option<&str>,
    ) -> Result<(), IdentityError>;

    /// Sets or generates the stream password; the generated value is returned
    /// once.
    async fn change_stream_password(
        &self,
        caster_id: &str,
        password: Option<&str>,
    ) -> Result<Option<String>, IdentityError>;
}

/// HTTP client for the scheduler `/internal/*` API.
pub struct HttpIdentityApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpIdentityApi {
    /// Builds a client against `base_url` (e.g. `http://scheduler:8080`).
    pub fn new(
        base_url: impl Into<String>,
        service_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            service_token: service_token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/internal{path}", self.base_url)
    }

    /// Maps a non-success response into an [`IdentityError`].
    async fn error_from(response: reqwest::Response) -> IdentityError {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        let field = body["error"]["details"]["field"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        match status.as_u16() {
            401 => IdentityError::InvalidCredentials,
            404 => IdentityError::NotFound,
            422 => IdentityError::Validation { field, message },
            other => IdentityError::Upstream(format!("scheduler returned {other}: {message}")),
        }
    }
}

#[async_trait]
impl IdentityApi for HttpIdentityApi {
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        let response = self
            .client
            .post(self.url("/auth/verify"))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({ "username": username, "password": password }))
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        let body: VerifyResponse = response
            .json()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        Ok(body.caster)
    }

    async fn create_session(
        &self,
        caster_id: &str,
        ip_address: Option<&str>,
        user_agent: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        let response = self
            .client
            .post(self.url("/sessions"))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({
                "caster_id": caster_id,
                "ip_address": ip_address,
                "user_agent": user_agent,
            }))
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))
    }

    async fn validate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError> {
        let response = self
            .client
            .get(self.url("/sessions/current"))
            .bearer_auth(&self.service_token)
            .header("x-session-token", token)
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        let body = response
            .json()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        Ok(Some(body))
    }

    async fn revoke_session(&self, session_id: &str) -> Result<(), IdentityError> {
        let response = self
            .client
            .delete(self.url(&format!("/sessions/{session_id}")))
            .bearer_auth(&self.service_token)
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error_from(response).await)
        }
    }

    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError> {
        let response = self
            .client
            .get(self.url(&format!("/casters/{caster_id}")))
            .bearer_auth(&self.service_token)
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))
    }

    async fn change_password(
        &self,
        caster_id: &str,
        password: &str,
        keep_session_id: Option<&str>,
    ) -> Result<(), IdentityError> {
        let response = self
            .client
            .post(self.url(&format!("/casters/{caster_id}/password")))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({
                "password": password,
                "keep_session_id": keep_session_id,
            }))
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error_from(response).await)
        }
    }

    async fn change_stream_password(
        &self,
        caster_id: &str,
        password: Option<&str>,
    ) -> Result<Option<String>, IdentityError> {
        let response = self
            .client
            .post(self.url(&format!("/casters/{caster_id}/stream-password")))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({ "password": password }))
            .send()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        let body: StreamPasswordResponse = response
            .json()
            .await
            .map_err(|e| IdentityError::Upstream(e.to_string()))?;
        Ok(body.password)
    }
}

#[derive(Debug, Deserialize)]
struct VerifyResponse {
    caster: CasterDto,
}

#[derive(Debug, Deserialize)]
struct StreamPasswordResponse {
    #[serde(default)]
    password: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #18: upload jobs (write-through to `/internal/upload-jobs`, §4.2)
// ---------------------------------------------------------------------------

/// Body of `POST /internal/upload-jobs`.
#[derive(Debug, Clone)]
pub struct NewUploadJob {
    pub caster_id: String,
    pub filename: String,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
}

/// Body of `PATCH /internal/upload-jobs/{id}`.
#[derive(Debug, Clone)]
pub struct UploadJobPatch {
    pub state: String,
    pub sha256: Option<String>,
    pub track_id: Option<String>,
    pub error: Option<String>,
}

/// An upload job as returned by the scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadJob {
    pub id: String,
    #[serde(default)]
    pub caster_id: Option<String>,
    pub filename: String,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub sha256: Option<String>,
    pub state: String,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Upload-job operation failure.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct JobError(#[from] JobErrorKind);

#[derive(Debug, thiserror::Error)]
enum JobErrorKind {
    #[error("job not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("{message}")]
    Validation { field: String, message: String },
    #[error("upstream error: {0}")]
    Upstream(String),
}

impl JobError {
    /// `true` when the job does not exist (`404`).
    pub fn is_not_found(&self) -> bool {
        matches!(self.0, JobErrorKind::NotFound)
    }

    /// A `404 not found` job error.
    pub fn not_found() -> Self {
        JobErrorKind::NotFound.into()
    }
}

/// Upload-job write-through operations. Implemented by [`HttpUploadJobApi`] in
/// production and by a fake in tests.
#[async_trait]
pub trait UploadJobApi: Send + Sync {
    /// Creates a `pending` job; returns its id.
    async fn create_job(&self, job: &NewUploadJob) -> Result<String, JobError>;

    /// Applies a state patch and returns the updated job.
    async fn patch_job(&self, id: &str, patch: &UploadJobPatch) -> Result<UploadJob, JobError>;

    /// Fetches one job (for polling fragments).
    async fn get_job(&self, id: &str) -> Result<UploadJob, JobError>;

    /// Lists recent jobs, newest first.
    async fn list_jobs(&self, limit: i64) -> Result<Vec<UploadJob>, JobError>;
}

/// HTTP client for `/internal/upload-jobs` (service-token bearer).
pub struct HttpUploadJobApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpUploadJobApi {
    /// Builds a client against `base_url` (e.g. `http://scheduler:8080`).
    pub fn new(
        base_url: impl Into<String>,
        service_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            service_token: service_token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/internal{path}", self.base_url)
    }

    async fn error_from(response: reqwest::Response) -> JobError {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        let field = body["error"]["details"]["field"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        match status.as_u16() {
            404 => JobErrorKind::NotFound.into(),
            409 => JobErrorKind::Conflict(message).into(),
            422 => JobErrorKind::Validation { field, message }.into(),
            other => {
                JobErrorKind::Upstream(format!("scheduler returned {other}: {message}")).into()
            }
        }
    }
}

#[async_trait]
impl UploadJobApi for HttpUploadJobApi {
    async fn create_job(&self, job: &NewUploadJob) -> Result<String, JobError> {
        let response = self
            .client
            .post(self.url("/upload-jobs"))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({
                "caster_id": job.caster_id,
                "filename": job.filename,
                "size_bytes": job.size_bytes,
                "sha256": job.sha256,
            }))
            .send()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        let body: CreateJobResponse = response
            .json()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()))?;
        Ok(body.id)
    }

    async fn patch_job(&self, id: &str, patch: &UploadJobPatch) -> Result<UploadJob, JobError> {
        let response = self
            .client
            .patch(self.url(&format!("/upload-jobs/{id}")))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({
                "state": patch.state,
                "sha256": patch.sha256,
                "track_id": patch.track_id,
                "error": patch.error,
            }))
            .send()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()).into())
    }

    async fn get_job(&self, id: &str) -> Result<UploadJob, JobError> {
        let response = self
            .client
            .get(self.url(&format!("/upload-jobs/{id}")))
            .bearer_auth(&self.service_token)
            .send()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()).into())
    }

    async fn list_jobs(&self, limit: i64) -> Result<Vec<UploadJob>, JobError> {
        let response = self
            .client
            .get(self.url("/upload-jobs"))
            .bearer_auth(&self.service_token)
            .query(&[("limit", limit.to_string())])
            .send()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| JobErrorKind::Upstream(e.to_string()).into())
    }
}

#[derive(Debug, Deserialize)]
struct CreateJobResponse {
    id: String,
}

// ---------------------------------------------------------------------------
// Issue #18: waveforms (write-through to `/internal/waveforms`, D8/C05)
// ---------------------------------------------------------------------------

/// Body of `POST /internal/waveforms` — one waveform upsert.
#[derive(Debug, Clone)]
pub struct NewWaveform {
    /// `track` | `tape`.
    pub subject_kind: String,
    pub subject_id: String,
    pub scheme: i64,
    pub algorithm: String,
    pub ms_per_bar: i64,
    pub bars: i64,
    /// `2 * bars` bytes of interleaved 8-bit `(min, max)` pairs.
    pub data: Vec<u8>,
}

/// A waveform as returned by the scheduler (`data` is a JSON byte array).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Waveform {
    pub subject_kind: String,
    pub subject_id: String,
    pub scheme: i64,
    pub algorithm: String,
    pub ms_per_bar: i64,
    pub bars: i64,
    #[serde(default)]
    pub data: Vec<u8>,
    pub created_at: String,
}

/// Waveform operation failure.
#[derive(Debug, thiserror::Error)]
pub enum WaveformApiError {
    /// The waveform does not exist (HTTP `404`).
    #[error("waveform not found")]
    NotFound,
    /// Transport/parse failure or unexpected status.
    #[error("upstream error: {0}")]
    Upstream(String),
}

/// Waveform write-through operations. Implemented by [`HttpWaveformApi`] in
/// production and by a fake in tests.
#[async_trait]
pub trait WaveformApi: Send + Sync {
    /// Upserts the waveform for `(subject_kind, subject_id)`.
    async fn put_waveform(&self, waveform: &NewWaveform) -> Result<Waveform, WaveformApiError>;

    /// Fetches one waveform; `Ok(None)` on `404`.
    async fn get_waveform(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError>;
}

/// HTTP client for `/internal/waveforms` (service-token bearer).
pub struct HttpWaveformApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpWaveformApi {
    /// Builds a client against `base_url` (e.g. `http://scheduler:8080`).
    pub fn new(
        base_url: impl Into<String>,
        service_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            service_token: service_token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/internal{path}", self.base_url)
    }
}

#[async_trait]
impl WaveformApi for HttpWaveformApi {
    async fn put_waveform(&self, waveform: &NewWaveform) -> Result<Waveform, WaveformApiError> {
        let response = self
            .client
            .post(self.url("/waveforms"))
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({
                "subject_kind": waveform.subject_kind,
                "subject_id": waveform.subject_id,
                "scheme": waveform.scheme,
                "algorithm": waveform.algorithm,
                "ms_per_bar": waveform.ms_per_bar,
                "bars": waveform.bars,
                "data": waveform.data,
            }))
            .send()
            .await
            .map_err(|e| WaveformApiError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(WaveformApiError::Upstream(format!(
                "scheduler returned {}",
                response.status()
            )));
        }
        response
            .json()
            .await
            .map_err(|e| WaveformApiError::Upstream(e.to_string()))
    }

    async fn get_waveform(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError> {
        let response = self
            .client
            .get(self.url(&format!("/waveforms/{subject_kind}/{subject_id}")))
            .bearer_auth(&self.service_token)
            .send()
            .await
            .map_err(|e| WaveformApiError::Upstream(e.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(WaveformApiError::Upstream(format!(
                "scheduler returned {}",
                response.status()
            )));
        }
        let waveform = response
            .json()
            .await
            .map_err(|e| WaveformApiError::Upstream(e.to_string()))?;
        Ok(Some(waveform))
    }
}

// ---------------------------------------------------------------------------
// Issue #19: tracks (Library) — read/write to `/api/v1/tracks*`, §4.2
// ---------------------------------------------------------------------------

/// A library track as returned by the scheduler (`tracks` row).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub music_api_id: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: i64,
    pub storage_key: String,
    #[serde(default)]
    pub artwork_url: Option<String>,
    pub visibility: String,
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Query parameters of `GET /tracks` (all optional).
#[derive(Debug, Clone, Default)]
pub struct TrackQuery {
    pub q: Option<String>,
    pub visibility: Option<String>,
    pub owner_caster_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Body of `POST /tracks` — the local projection of a Music-API track.
#[derive(Debug, Clone, Default)]
pub struct NewTrackRequest {
    pub music_api_id: String,
    pub title: String,
    pub storage_key: String,
    pub duration_ms: i64,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
    pub visibility: Option<String>,
    pub caster_id: Option<String>,
}

/// Track operation failure.
#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    /// Unknown or foreign-private track (scheduler `404`).
    #[error("not found")]
    NotFound,
    /// Duplicate `music_api_id` (scheduler `409`).
    #[error("conflict: {0}")]
    Conflict(String),
    /// Field-level rejection (scheduler `422`).
    #[error("{message}")]
    Validation { field: String, message: String },
    /// Transport/parse failure or unexpected status.
    #[error("upstream error: {0}")]
    Upstream(String),
}

impl TrackError {
    /// `true` on a `404` (unknown *or* foreign-private).
    pub fn is_not_found(&self) -> bool {
        matches!(self, TrackError::NotFound)
    }

    /// `true` on a `409` (duplicate `music_api_id`).
    pub fn is_conflict(&self) -> bool {
        matches!(self, TrackError::Conflict(_))
    }
}

/// Track library operations. Implemented by [`HttpTrackApi`] in production and
/// by [`FakeTrackApi`] in tests. `viewer` is the session `caster_id`; it is
/// sent as `X-Caster-Id` and must never come from client input (§1).
#[async_trait]
pub trait TrackApi: Send + Sync {
    /// Lists/searches tracks within the viewer's visibility scope.
    async fn list(
        &self,
        viewer: Option<&str>,
        query: &TrackQuery,
    ) -> Result<Vec<Track>, TrackError>;

    /// Fetches one track within scope (foreign-private → `NotFound`).
    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Track, TrackError>;

    /// Registers a new track (duplicate `music_api_id` → `Conflict`).
    async fn create(
        &self,
        viewer: Option<&str>,
        req: &NewTrackRequest,
    ) -> Result<Track, TrackError>;

    /// Sets a track's visibility (`shared`|`private`); owner-scoped.
    async fn set_visibility(
        &self,
        viewer: Option<&str>,
        id: &str,
        visibility: &str,
    ) -> Result<Track, TrackError>;

    /// Deletes a track within scope.
    async fn delete(&self, viewer: Option<&str>, id: &str) -> Result<(), TrackError>;
}

/// HTTP client for `/api/v1/tracks*` (service-token bearer + `X-Caster-Id`).
pub struct HttpTrackApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpTrackApi {
    /// Builds a client against `base_url` (e.g. `http://scheduler:8080`).
    pub fn new(
        base_url: impl Into<String>,
        service_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            service_token: service_token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base_url)
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        viewer: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, self.url(path))
            .bearer_auth(&self.service_token);
        if let Some(viewer) = viewer {
            builder = builder.header("x-caster-id", viewer);
        }
        builder
    }

    async fn error_from(response: reqwest::Response) -> TrackError {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        let field = body["error"]["details"]["field"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        match status.as_u16() {
            404 => TrackError::NotFound,
            409 => TrackError::Conflict(message),
            422 => TrackError::Validation { field, message },
            other => TrackError::Upstream(format!("scheduler returned {other}: {message}")),
        }
    }
}

#[async_trait]
impl TrackApi for HttpTrackApi {
    async fn list(
        &self,
        viewer: Option<&str>,
        query: &TrackQuery,
    ) -> Result<Vec<Track>, TrackError> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(q) = &query.q {
            params.push(("q", q.clone()));
        }
        if let Some(v) = &query.visibility {
            params.push(("visibility", v.clone()));
        }
        if let Some(o) = &query.owner_caster_id {
            params.push(("owner_caster_id", o.clone()));
        }
        if let Some(l) = query.limit {
            params.push(("limit", l.to_string()));
        }
        if let Some(o) = query.offset {
            params.push(("offset", o.to_string()));
        }
        let response = self
            .request(reqwest::Method::GET, "/tracks", viewer)
            .query(&params)
            .send()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))
    }

    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Track, TrackError> {
        let response = self
            .request(reqwest::Method::GET, &format!("/tracks/{id}"), viewer)
            .send()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))
    }

    async fn create(
        &self,
        viewer: Option<&str>,
        req: &NewTrackRequest,
    ) -> Result<Track, TrackError> {
        let response = self
            .request(reqwest::Method::POST, "/tracks", viewer)
            .json(&serde_json::json!({
                "music_api_id": req.music_api_id,
                "title": req.title,
                "storage_key": req.storage_key,
                "duration_ms": req.duration_ms,
                "artist": req.artist,
                "album": req.album,
                "artwork_url": req.artwork_url,
                "visibility": req.visibility,
                "caster_id": req.caster_id,
            }))
            .send()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))
    }

    async fn set_visibility(
        &self,
        viewer: Option<&str>,
        id: &str,
        visibility: &str,
    ) -> Result<Track, TrackError> {
        let response = self
            .request(reqwest::Method::PATCH, &format!("/tracks/{id}"), viewer)
            .json(&serde_json::json!({ "visibility": visibility }))
            .send()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))
    }

    async fn delete(&self, viewer: Option<&str>, id: &str) -> Result<(), TrackError> {
        let response = self
            .request(reqwest::Method::DELETE, &format!("/tracks/{id}"), viewer)
            .send()
            .await
            .map_err(|e| TrackError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error_from(response).await)
        }
    }
}

/// In-memory [`TrackApi`] for tests. It enforces the same §1 visibility scope
/// as the scheduler so portal behaviour is exercised faithfully without a
/// network.
#[derive(Default)]
pub struct FakeTrackApi {
    inner: std::sync::Mutex<Vec<Track>>,
    counter: std::sync::Mutex<u64>,
}

impl FakeTrackApi {
    /// A fresh, empty fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a track directly (bypasses `create`).
    pub fn seed(&self, track: Track) {
        self.inner.lock().unwrap().push(track);
    }

    /// All tracks, regardless of scope (test assertions).
    pub fn all(&self) -> Vec<Track> {
        self.inner.lock().unwrap().clone()
    }

    fn visible(track: &Track, viewer: Option<&str>) -> bool {
        track.visibility == "shared" || Some(track.caster_id.as_deref()) == Some(viewer)
    }
}

#[async_trait]
impl TrackApi for FakeTrackApi {
    async fn list(
        &self,
        viewer: Option<&str>,
        query: &TrackQuery,
    ) -> Result<Vec<Track>, TrackError> {
        let tracks = self.inner.lock().unwrap();
        let matches: Vec<Track> = tracks
            .iter()
            .filter(|t| Self::visible(t, viewer))
            .filter(|t| {
                query.q.as_deref().is_none_or(|q| {
                    let q = q.to_lowercase();
                    t.title.to_lowercase().contains(&q)
                        || t.artist.to_lowercase().contains(&q)
                        || t.album.to_lowercase().contains(&q)
                })
            })
            .filter(|t| {
                query
                    .visibility
                    .as_deref()
                    .is_none_or(|v| t.visibility == v)
            })
            .filter(|t| {
                query
                    .owner_caster_id
                    .as_deref()
                    .is_none_or(|o| t.caster_id.as_deref() == Some(o))
            })
            .cloned()
            .collect();
        Ok(matches)
    }

    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Track, TrackError> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id && Self::visible(t, viewer))
            .cloned()
            .ok_or(TrackError::NotFound)
    }

    async fn create(
        &self,
        _viewer: Option<&str>,
        req: &NewTrackRequest,
    ) -> Result<Track, TrackError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.iter().any(|t| t.music_api_id == req.music_api_id) {
            return Err(TrackError::Conflict(
                "a track with this music_api_id already exists".into(),
            ));
        }
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let track = Track {
            id: format!("local-{counter}"),
            music_api_id: req.music_api_id.clone(),
            title: req.title.clone(),
            artist: req.artist.clone().unwrap_or_default(),
            album: req.album.clone().unwrap_or_default(),
            duration_ms: req.duration_ms,
            storage_key: req.storage_key.clone(),
            artwork_url: req.artwork_url.clone(),
            visibility: req
                .visibility
                .clone()
                .unwrap_or_else(|| "shared".to_string()),
            caster_id: req.caster_id.clone(),
            created_at: "2026-11-18T12:00:00.000Z".into(),
            updated_at: "2026-11-18T12:00:00.000Z".into(),
        };
        inner.push(track.clone());
        Ok(track)
    }

    async fn set_visibility(
        &self,
        viewer: Option<&str>,
        id: &str,
        visibility: &str,
    ) -> Result<Track, TrackError> {
        let mut inner = self.inner.lock().unwrap();
        let track = inner
            .iter_mut()
            .find(|t| t.id == id && Self::visible(t, viewer))
            .ok_or(TrackError::NotFound)?;
        // Mirror the scheduler: PATCH is owner-only, even for shared tracks.
        if Some(track.caster_id.as_deref()) != Some(viewer) {
            return Err(TrackError::NotFound);
        }
        track.visibility = visibility.to_string();
        Ok(track.clone())
    }

    async fn delete(&self, viewer: Option<&str>, id: &str) -> Result<(), TrackError> {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.len();
        inner.retain(|t| !(t.id == id && Self::visible(t, viewer)));
        if inner.len() == before {
            return Err(TrackError::NotFound);
        }
        Ok(())
    }
}
