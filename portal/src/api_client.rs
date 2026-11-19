//! Typed client for the scheduler's identity authority (`/internal/*`, §4.2).
//!
//! The portal is a thin UI: every identity operation is a call to the
//! scheduler, authenticated with `Authorization: Bearer $SCHEDULER_SERVICE_TOKEN`.
//! The `IdentityApi` trait keeps the handlers testable with a fake backend.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
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
            // Mirror the scheduler's `ORDER BY` pagination: honour offset/limit
            // so the fake cannot mask paging bugs in the portal.
            .skip(query.offset.unwrap_or(0).max(0) as usize)
            .take(query.limit.unwrap_or(i64::MAX).max(0) as usize)
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

// ---------------------------------------------------------------------------
// Issue #20: queues (Queue-UI) — read/write to `/api/v1/queues*`, §4.2
// ---------------------------------------------------------------------------

/// A fixed timestamp for fakes (never rendered as "now").
const FAKE_TS: &str = "2026-11-19T12:00:00.000Z";

/// A queue as returned by the scheduler (`queues` row, §3.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Queue {
    pub id: String,
    pub caster_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `background` | `slot`.
    pub kind: String,
    pub version: i64,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// A queue item as returned by the scheduler (`queue_items` row, §3.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueItem {
    pub id: String,
    pub queue_id: String,
    pub position: i64,
    /// `track` | `tape` | `link`.
    pub kind: String,
    #[serde(default)]
    pub track_id: Option<String>,
    #[serde(default)]
    pub tape_id: Option<String>,
    #[serde(default)]
    pub link_url: Option<String>,
    /// `youtube` | `spotify` | `other`.
    #[serde(default)]
    pub link_provider: Option<String>,
    /// `pending` | `ready` | `failed` | `unplayable`.
    #[serde(default)]
    pub link_state: Option<String>,
    #[serde(default)]
    pub title_override: Option<String>,
    /// `pending` | `playing` | `played` | `skipped`.
    pub state: String,
    #[serde(default)]
    pub played_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

/// Query parameters of `GET /queues` (both optional).
#[derive(Debug, Clone, Default)]
pub struct QueueQuery {
    pub caster_id: Option<String>,
    pub kind: Option<String>,
}

/// Body of `POST /queues`.
#[derive(Debug, Clone, Default)]
pub struct NewQueue {
    pub caster_id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: Option<String>,
}

/// Body of `POST /queues/{id}/items`.
#[derive(Debug, Clone, Default)]
pub struct NewQueueItem {
    pub kind: String,
    pub track_id: Option<String>,
    pub tape_id: Option<String>,
    pub link_url: Option<String>,
    pub link_provider: Option<String>,
    pub title_override: Option<String>,
    pub position: Option<i64>,
}

/// Queue operation failure (mapping identical to [`TrackError`]: `404`/`409`/`422`).
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// Unknown queue/item (scheduler `404`).
    #[error("not found")]
    NotFound,
    /// Optimistic-lock or duplicate conflict (scheduler `409`).
    #[error("conflict: {0}")]
    Conflict(String),
    /// Field-level rejection (scheduler `422`).
    #[error("{message}")]
    Validation { field: String, message: String },
    /// Transport/parse failure or unexpected status.
    #[error("upstream error: {0}")]
    Upstream(String),
}

impl QueueError {
    /// `true` on a `404`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, QueueError::NotFound)
    }

    /// `true` on a `409` (version conflict / duplicate).
    pub fn is_conflict(&self) -> bool {
        matches!(self, QueueError::Conflict(_))
    }
}

/// Queue operations. Implemented by [`HttpQueueApi`] in production and by
/// [`FakeQueueApi`] in tests. `viewer` is the session `caster_id`; it is sent as
/// `X-Caster-Id` and must never come from client input (§1).
#[async_trait]
pub trait QueueApi: Send + Sync {
    /// Lists queues, optionally filtered by owner/kind.
    async fn list(
        &self,
        viewer: Option<&str>,
        query: &QueueQuery,
    ) -> Result<Vec<Queue>, QueueError>;

    /// Fetches one queue.
    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Queue, QueueError>;

    /// Creates a queue (`version = 1`).
    async fn create(&self, viewer: Option<&str>, req: &NewQueue) -> Result<Queue, QueueError>;

    /// Renames/updates a queue with an optimistic-lock `version`.
    async fn update(
        &self,
        viewer: Option<&str>,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        version: i64,
    ) -> Result<Queue, QueueError>;

    /// Deletes a queue with an optimistic-lock `version`.
    async fn delete(&self, viewer: Option<&str>, id: &str, version: i64) -> Result<(), QueueError>;

    /// Lists a queue's items in position order.
    async fn items(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
    ) -> Result<Vec<QueueItem>, QueueError>;

    /// Adds an item to a queue.
    async fn add_item(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        req: &NewQueueItem,
    ) -> Result<QueueItem, QueueError>;

    /// Updates mutable item fields (`title_override`, `state`, `position`).
    async fn update_item(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        item_id: &str,
        title_override: Option<&str>,
        state: Option<&str>,
        position: Option<i64>,
    ) -> Result<QueueItem, QueueError>;

    /// Deletes an item.
    async fn delete_item(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        item_id: &str,
    ) -> Result<(), QueueError>;

    /// Reorders all items (exact id set) with an optimistic-lock `version`.
    async fn reorder(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        item_ids: &[String],
        version: i64,
    ) -> Result<Queue, QueueError>;

    /// Removes every item from a queue.
    async fn clear(&self, viewer: Option<&str>, queue_id: &str) -> Result<(), QueueError>;
}

/// HTTP client for `/api/v1/queues*` (service-token bearer + `X-Caster-Id`).
pub struct HttpQueueApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpQueueApi {
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

    async fn error_from(response: reqwest::Response) -> QueueError {
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
            404 => QueueError::NotFound,
            409 => QueueError::Conflict(message),
            422 => QueueError::Validation { field, message },
            other => QueueError::Upstream(format!("scheduler returned {other}: {message}")),
        }
    }
}

#[async_trait]
impl QueueApi for HttpQueueApi {
    async fn list(
        &self,
        viewer: Option<&str>,
        query: &QueueQuery,
    ) -> Result<Vec<Queue>, QueueError> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(c) = &query.caster_id {
            params.push(("caster_id", c.clone()));
        }
        if let Some(k) = &query.kind {
            params.push(("kind", k.clone()));
        }
        let response = self
            .request(reqwest::Method::GET, "/queues", viewer)
            .query(&params)
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Queue, QueueError> {
        let response = self
            .request(reqwest::Method::GET, &format!("/queues/{id}"), viewer)
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn create(&self, viewer: Option<&str>, req: &NewQueue) -> Result<Queue, QueueError> {
        let response = self
            .request(reqwest::Method::POST, "/queues", viewer)
            .json(&serde_json::json!({
                "caster_id": req.caster_id,
                "name": req.name,
                "description": req.description,
                "kind": req.kind,
            }))
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn update(
        &self,
        viewer: Option<&str>,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        version: i64,
    ) -> Result<Queue, QueueError> {
        let response = self
            .request(reqwest::Method::PATCH, &format!("/queues/{id}"), viewer)
            .json(&serde_json::json!({
                "name": name,
                "description": description,
                "version": version,
            }))
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn delete(&self, viewer: Option<&str>, id: &str, version: i64) -> Result<(), QueueError> {
        let response = self
            .request(reqwest::Method::DELETE, &format!("/queues/{id}"), viewer)
            .json(&serde_json::json!({ "version": version }))
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error_from(response).await)
        }
    }

    async fn items(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
    ) -> Result<Vec<QueueItem>, QueueError> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("/queues/{queue_id}/items"),
                viewer,
            )
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn add_item(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        req: &NewQueueItem,
    ) -> Result<QueueItem, QueueError> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/queues/{queue_id}/items"),
                viewer,
            )
            .json(&serde_json::json!({
                "kind": req.kind,
                "track_id": req.track_id,
                "tape_id": req.tape_id,
                "link_url": req.link_url,
                "link_provider": req.link_provider,
                "title_override": req.title_override,
                "position": req.position,
            }))
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn update_item(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        item_id: &str,
        title_override: Option<&str>,
        state: Option<&str>,
        position: Option<i64>,
    ) -> Result<QueueItem, QueueError> {
        let response = self
            .request(
                reqwest::Method::PATCH,
                &format!("/queues/{queue_id}/items/{item_id}"),
                viewer,
            )
            .json(&serde_json::json!({
                "title_override": title_override,
                "state": state,
                "position": position,
            }))
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn delete_item(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        item_id: &str,
    ) -> Result<(), QueueError> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                &format!("/queues/{queue_id}/items/{item_id}"),
                viewer,
            )
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error_from(response).await)
        }
    }

    async fn reorder(
        &self,
        viewer: Option<&str>,
        queue_id: &str,
        item_ids: &[String],
        version: i64,
    ) -> Result<Queue, QueueError> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/queues/{queue_id}/reorder"),
                viewer,
            )
            .json(&serde_json::json!({
                "item_ids": item_ids,
                "version": version,
            }))
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))
    }

    async fn clear(&self, viewer: Option<&str>, queue_id: &str) -> Result<(), QueueError> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/queues/{queue_id}/clear"),
                viewer,
            )
            .send()
            .await
            .map_err(|e| QueueError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::error_from(response).await)
        }
    }
}

/// Enforces the "exactly one content field" matrix from §3.3 (shared by the fake).
fn validate_item_content(
    kind: &str,
    track_id: Option<&str>,
    tape_id: Option<&str>,
    link_url: Option<&str>,
) -> Result<(), QueueError> {
    let (has_track, has_tape, has_link) =
        (track_id.is_some(), tape_id.is_some(), link_url.is_some());
    let ok = match kind {
        "track" => has_track && !has_tape && !has_link,
        "tape" => has_tape && !has_track && !has_link,
        "link" => has_link && !has_track && !has_tape,
        other => {
            return Err(QueueError::Validation {
                field: "kind".into(),
                message: format!("unknown kind '{other}', expected 'track', 'tape' or 'link'"),
            });
        }
    };
    if ok {
        Ok(())
    } else {
        Err(QueueError::Validation {
            field: "kind".into(),
            message: format!("kind '{kind}' requires exactly one matching content field"),
        })
    }
}

/// In-memory [`QueueApi`] for tests. It enforces the same invariants as the
/// scheduler (exactly one content field, monotonically increasing `position`, a
/// `version` bump on every mutation and exact item-set validation on `reorder`)
/// so portal behaviour is exercised faithfully without a network.
#[derive(Default)]
pub struct FakeQueueApi {
    queues: std::sync::Mutex<Vec<Queue>>,
    items: std::sync::Mutex<Vec<QueueItem>>,
    counter: std::sync::Mutex<u64>,
}

impl FakeQueueApi {
    /// A fresh, empty fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a queue directly (bypasses `create`).
    pub fn seed_queue(&self, queue: Queue) {
        self.queues.lock().unwrap().push(queue);
    }

    /// Seeds an item directly (bypasses `add_item`).
    pub fn seed_item(&self, item: QueueItem) {
        self.items.lock().unwrap().push(item);
    }

    /// All queues (test assertions).
    pub fn all_queues(&self) -> Vec<Queue> {
        self.queues.lock().unwrap().clone()
    }

    /// All items (test assertions).
    pub fn all_items(&self) -> Vec<QueueItem> {
        self.items.lock().unwrap().clone()
    }

    fn next_id(&self, prefix: &str) -> String {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        format!("{prefix}-{counter}")
    }

    fn find_queue(&self, id: &str) -> Result<Queue, QueueError> {
        self.queues
            .lock()
            .unwrap()
            .iter()
            .find(|q| q.id == id)
            .cloned()
            .ok_or(QueueError::NotFound)
    }

    fn bump(&self, queue_id: &str) {
        if let Some(q) = self
            .queues
            .lock()
            .unwrap()
            .iter_mut()
            .find(|q| q.id == queue_id)
        {
            q.version += 1;
        }
    }

    fn items_for(&self, queue_id: &str) -> Vec<QueueItem> {
        let mut items: Vec<QueueItem> = self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.queue_id == queue_id)
            .cloned()
            .collect();
        items.sort_by(|a, b| a.position.cmp(&b.position).then(a.id.cmp(&b.id)));
        items
    }
}

#[async_trait]
impl QueueApi for FakeQueueApi {
    async fn list(
        &self,
        _viewer: Option<&str>,
        query: &QueueQuery,
    ) -> Result<Vec<Queue>, QueueError> {
        let queues = self.queues.lock().unwrap();
        Ok(queues
            .iter()
            .filter(|q| query.caster_id.as_deref().is_none_or(|c| q.caster_id == c))
            .filter(|q| query.kind.as_deref().is_none_or(|k| q.kind == k))
            .cloned()
            .collect())
    }

    async fn get(&self, _viewer: Option<&str>, id: &str) -> Result<Queue, QueueError> {
        self.find_queue(id)
    }

    async fn create(&self, _viewer: Option<&str>, req: &NewQueue) -> Result<Queue, QueueError> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err(QueueError::Validation {
                field: "name".into(),
                message: "name must not be empty".into(),
            });
        }
        let queue = Queue {
            id: self.next_id("q"),
            caster_id: req.caster_id.clone(),
            name: name.to_string(),
            description: req.description.clone().unwrap_or_default(),
            kind: req.kind.clone().unwrap_or_else(|| "background".into()),
            version: 1,
            created_at: FAKE_TS.into(),
            updated_at: FAKE_TS.into(),
        };
        self.queues.lock().unwrap().push(queue.clone());
        Ok(queue)
    }

    async fn update(
        &self,
        _viewer: Option<&str>,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        version: i64,
    ) -> Result<Queue, QueueError> {
        let mut queues = self.queues.lock().unwrap();
        let queue = queues
            .iter_mut()
            .find(|q| q.id == id)
            .ok_or(QueueError::NotFound)?;
        if queue.version != version {
            return Err(QueueError::Conflict("version_conflict".into()));
        }
        if let Some(name) = name {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(QueueError::Validation {
                    field: "name".into(),
                    message: "name must not be empty".into(),
                });
            }
            queue.name = trimmed.to_string();
        }
        if let Some(description) = description {
            queue.description = description.to_string();
        }
        queue.version += 1;
        Ok(queue.clone())
    }

    async fn delete(
        &self,
        _viewer: Option<&str>,
        id: &str,
        version: i64,
    ) -> Result<(), QueueError> {
        let mut queues = self.queues.lock().unwrap();
        let index = queues
            .iter()
            .position(|q| q.id == id)
            .ok_or(QueueError::NotFound)?;
        if queues[index].version != version {
            return Err(QueueError::Conflict("version_conflict".into()));
        }
        queues.remove(index);
        self.items.lock().unwrap().retain(|i| i.queue_id != id);
        Ok(())
    }

    async fn items(
        &self,
        _viewer: Option<&str>,
        queue_id: &str,
    ) -> Result<Vec<QueueItem>, QueueError> {
        self.find_queue(queue_id)?;
        Ok(self.items_for(queue_id))
    }

    async fn add_item(
        &self,
        _viewer: Option<&str>,
        queue_id: &str,
        req: &NewQueueItem,
    ) -> Result<QueueItem, QueueError> {
        self.find_queue(queue_id)?;
        validate_item_content(
            &req.kind,
            req.track_id.as_deref(),
            req.tape_id.as_deref(),
            req.link_url.as_deref(),
        )?;
        if let Some(position) = req.position {
            if position < 0 {
                return Err(QueueError::Validation {
                    field: "position".into(),
                    message: "position must be >= 0".into(),
                });
            }
        }
        let id = self.next_id("i");
        let item = {
            let mut items = self.items.lock().unwrap();
            let position = match req.position {
                Some(position) => position,
                None => items
                    .iter()
                    .filter(|i| i.queue_id == queue_id)
                    .map(|i| i.position)
                    .max()
                    .map_or(0, |m| m + 1),
            };
            let item = QueueItem {
                id,
                queue_id: queue_id.to_string(),
                position,
                kind: req.kind.clone(),
                track_id: req.track_id.clone(),
                tape_id: req.tape_id.clone(),
                link_url: req.link_url.clone(),
                link_provider: req.link_provider.clone(),
                link_state: (req.kind == "link").then(|| "pending".to_string()),
                title_override: req.title_override.clone(),
                state: "pending".into(),
                played_at: None,
                created_at: FAKE_TS.into(),
            };
            items.push(item.clone());
            item
        };
        self.bump(queue_id);
        Ok(item)
    }

    async fn update_item(
        &self,
        _viewer: Option<&str>,
        queue_id: &str,
        item_id: &str,
        title_override: Option<&str>,
        state: Option<&str>,
        position: Option<i64>,
    ) -> Result<QueueItem, QueueError> {
        self.find_queue(queue_id)?;
        let item = {
            let mut items = self.items.lock().unwrap();
            let index = items
                .iter()
                .position(|i| i.id == item_id && i.queue_id == queue_id)
                .ok_or(QueueError::NotFound)?;
            if let Some(title) = title_override {
                items[index].title_override = Some(title.to_string());
            }
            if let Some(state) = state {
                items[index].state = state.to_string();
            }
            if let Some(position) = position {
                items[index].position = position;
            }
            items[index].clone()
        };
        self.bump(queue_id);
        Ok(item)
    }

    async fn delete_item(
        &self,
        _viewer: Option<&str>,
        queue_id: &str,
        item_id: &str,
    ) -> Result<(), QueueError> {
        self.find_queue(queue_id)?;
        {
            let mut items = self.items.lock().unwrap();
            let before = items.len();
            items.retain(|i| !(i.id == item_id && i.queue_id == queue_id));
            if items.len() == before {
                return Err(QueueError::NotFound);
            }
        }
        self.bump(queue_id);
        Ok(())
    }

    async fn reorder(
        &self,
        _viewer: Option<&str>,
        queue_id: &str,
        item_ids: &[String],
        version: i64,
    ) -> Result<Queue, QueueError> {
        let queue = self.find_queue(queue_id)?;
        if queue.version != version {
            return Err(QueueError::Conflict("version_conflict".into()));
        }
        let mut current: Vec<String> = self.items_for(queue_id).into_iter().map(|i| i.id).collect();
        let mut requested: Vec<String> = item_ids.to_vec();
        current.sort();
        requested.sort();
        let mut deduped = requested.clone();
        deduped.dedup();
        if deduped.len() != requested.len() || requested != current {
            return Err(QueueError::Validation {
                field: "item_ids".into(),
                message: "item_ids must contain exactly the current item ids of this queue".into(),
            });
        }
        {
            let mut items = self.items.lock().unwrap();
            for (index, item_id) in item_ids.iter().enumerate() {
                if let Some(item) = items
                    .iter_mut()
                    .find(|i| &i.id == item_id && i.queue_id == queue_id)
                {
                    item.position = index as i64;
                }
            }
        }
        self.bump(queue_id);
        self.find_queue(queue_id)
    }

    async fn clear(&self, _viewer: Option<&str>, queue_id: &str) -> Result<(), QueueError> {
        self.find_queue(queue_id)?;
        self.items
            .lock()
            .unwrap()
            .retain(|i| i.queue_id != queue_id);
        self.bump(queue_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Issue #20/#22: control — `GET /api/v1/control/state` + `POST /control/*`, §6
// ---------------------------------------------------------------------------

/// Control-operation failure.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The scheduler rejected the action because live is active (`409
    /// live_active`), e.g. a pause while a caster is live.
    #[error("live active: {0}")]
    LiveActive(String),
    /// Any other rejection (`409`/`422`), e.g. a field error or nothing playing.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Transport/parse failure or unexpected status.
    #[error("upstream error: {0}")]
    Upstream(String),
}

/// One priority layer as returned by `GET /control/state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlSource {
    pub id: String,
    /// `live|show|queue|rotation|relay|fun`.
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub enabled: bool,
}

/// One connected caster (derived from `event_log`; often empty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedCaster {
    pub caster_id: String,
    #[serde(default)]
    pub mount: Option<String>,
    #[serde(default)]
    pub since: String,
}

/// The live-layer slice of [`ControlState`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LiveState {
    #[serde(default)]
    pub override_caster_id: Option<String>,
    #[serde(default)]
    pub active_caster_id: Option<String>,
    #[serde(default)]
    pub connected: Vec<ConnectedCaster>,
}

/// The currently playing item (`now_playing`), mirroring `PlaybackItemDto`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NowPlaying {
    #[serde(default)]
    pub queue_item_id: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub duration_ms: i64,
}

/// Full control-panel state (`GET /control/state`), mirroring the scheduler's
/// `ControlStateDto` (serde-compatible).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ControlState {
    #[serde(default)]
    pub sources: Vec<ControlSource>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub forced_layer: Option<String>,
    #[serde(default)]
    pub active_layer: Option<String>,
    #[serde(default)]
    pub derived: bool,
    #[serde(default)]
    pub live: LiveState,
    #[serde(default)]
    pub now_playing: Option<NowPlaying>,
}

/// Control operations the portal needs. Implemented by [`HttpControlApi`] in
/// production and by [`FakeControlApi`] in tests.
#[async_trait]
pub trait ControlApi: Send + Sync {
    /// Reads the full control-panel state. `viewer` is sent as `X-Caster-Id`.
    async fn state(&self, viewer: Option<&str>) -> Result<ControlState, ControlError>;

    /// Forces one of the six layers (`POST /control/switch`).
    async fn switch(&self, layer: &str, reason: &str) -> Result<(), ControlError>;

    /// Sets (`Some`) or clears (`None`) the live override (`POST /control/live`).
    async fn live(&self, caster_id: Option<&str>, reason: &str) -> Result<(), ControlError>;

    /// Pauses/resumes (`POST /control/pause`); live active ⇒ `LiveActive`.
    async fn pause(&self, paused: bool, reason: &str) -> Result<(), ControlError>;

    /// Requests a skip of the currently playing item.
    async fn skip(&self, reason: &str) -> Result<(), ControlError>;
}

/// HTTP client for `/api/v1/control/*` (service-token bearer).
pub struct HttpControlApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpControlApi {
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

    /// Maps a non-success response into a [`ControlError`].
    async fn error_from(response: reqwest::Response) -> ControlError {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        let code = body["error"]["code"].as_str().unwrap_or("").to_string();
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or("upstream error")
            .to_string();
        match (status.as_u16(), code.as_str()) {
            (409, "live_active") => ControlError::LiveActive(message),
            (409, _) | (422, _) => ControlError::Conflict(message),
            (other, _) => ControlError::Upstream(format!("scheduler returned {other}: {message}")),
        }
    }
}

#[async_trait]
impl ControlApi for HttpControlApi {
    async fn state(&self, viewer: Option<&str>) -> Result<ControlState, ControlError> {
        let response = self
            .request(reqwest::Method::GET, "/control/state", viewer)
            .send()
            .await
            .map_err(|e| ControlError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Self::error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ControlError::Upstream(e.to_string()))
    }

    async fn switch(&self, layer: &str, reason: &str) -> Result<(), ControlError> {
        self.post_action(
            "/control/switch",
            serde_json::json!({ "layer": layer, "reason": reason }),
        )
        .await
    }

    async fn live(&self, caster_id: Option<&str>, reason: &str) -> Result<(), ControlError> {
        self.post_action(
            "/control/live",
            serde_json::json!({ "caster_id": caster_id, "reason": reason }),
        )
        .await
    }

    async fn pause(&self, paused: bool, reason: &str) -> Result<(), ControlError> {
        self.post_action(
            "/control/pause",
            serde_json::json!({ "paused": paused, "reason": reason }),
        )
        .await
    }

    async fn skip(&self, reason: &str) -> Result<(), ControlError> {
        self.post_action("/control/skip", serde_json::json!({ "reason": reason }))
            .await
    }
}

impl HttpControlApi {
    /// Shared `POST /control/*` call: `204` success, otherwise mapped error.
    async fn post_action(&self, path: &str, body: serde_json::Value) -> Result<(), ControlError> {
        let response = self
            .request(reqwest::Method::POST, path, None)
            .json(&body)
            .send()
            .await
            .map_err(|e| ControlError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(Self::error_from(response).await)
    }
}

/// In-memory [`ControlApi`] for tests: holds a settable state, records the
/// write calls, and can simulate the `live_active` live guard.
#[derive(Default)]
pub struct FakeControlApi {
    calls: std::sync::Mutex<Vec<String>>,
    conflict: std::sync::Mutex<Option<String>>,
    state: std::sync::Mutex<ControlState>,
    switches: std::sync::Mutex<Vec<(String, String)>>,
    lives: std::sync::Mutex<Vec<(Option<String>, String)>>,
    pauses: std::sync::Mutex<Vec<(bool, String)>>,
    pause_live_active: std::sync::Mutex<bool>,
    state_error: std::sync::Mutex<bool>,
}

impl FakeControlApi {
    /// A fresh fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the state returned by [`ControlApi::state`].
    pub fn set_state(&self, state: ControlState) {
        *self.state.lock().unwrap() = state;
    }

    /// Makes every subsequent `skip` answer with a `409` conflict.
    pub fn fail_with(&self, message: &str) {
        *self.conflict.lock().unwrap() = Some(message.to_string());
    }

    /// Makes every subsequent `pause` answer with `409 live_active`.
    pub fn fail_pause_live(&self) {
        *self.pause_live_active.lock().unwrap() = true;
    }

    /// Makes every subsequent `state` answer fail (simulates an outage).
    pub fn fail_state(&self) {
        *self.state_error.lock().unwrap() = true;
    }

    /// The recorded skip reasons.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The recorded `(layer, reason)` switch calls.
    pub fn switches(&self) -> Vec<(String, String)> {
        self.switches.lock().unwrap().clone()
    }

    /// The recorded `(caster_id, reason)` live calls.
    pub fn lives(&self) -> Vec<(Option<String>, String)> {
        self.lives.lock().unwrap().clone()
    }

    /// The recorded `(paused, reason)` pause calls.
    pub fn pauses(&self) -> Vec<(bool, String)> {
        self.pauses.lock().unwrap().clone()
    }
}

#[async_trait]
impl ControlApi for FakeControlApi {
    async fn state(&self, _viewer: Option<&str>) -> Result<ControlState, ControlError> {
        if *self.state_error.lock().unwrap() {
            return Err(ControlError::Upstream("unreachable".into()));
        }
        Ok(self.state.lock().unwrap().clone())
    }

    async fn switch(&self, layer: &str, reason: &str) -> Result<(), ControlError> {
        self.switches
            .lock()
            .unwrap()
            .push((layer.to_string(), reason.to_string()));
        Ok(())
    }

    async fn live(&self, caster_id: Option<&str>, reason: &str) -> Result<(), ControlError> {
        self.lives
            .lock()
            .unwrap()
            .push((caster_id.map(str::to_string), reason.to_string()));
        Ok(())
    }

    async fn pause(&self, paused: bool, reason: &str) -> Result<(), ControlError> {
        self.pauses
            .lock()
            .unwrap()
            .push((paused, reason.to_string()));
        if paused && *self.pause_live_active.lock().unwrap() {
            return Err(ControlError::LiveActive("live active".into()));
        }
        Ok(())
    }

    async fn skip(&self, reason: &str) -> Result<(), ControlError> {
        self.calls.lock().unwrap().push(reason.to_string());
        if let Some(message) = self.conflict.lock().unwrap().clone() {
            return Err(ControlError::Conflict(message));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Issue #21: schedule, shows and broadcasts (calendar UI)
// ---------------------------------------------------------------------------

/// A soft scheduling conflict as returned by the scheduler (§4.2/§4.3).
/// The portal only renders these — it never computes conflicts itself (D12).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Warning {
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub broadcast_id: Option<String>,
    #[serde(default)]
    pub show_id: Option<String>,
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub starts_at: Option<String>,
    #[serde(default)]
    pub ends_at: Option<String>,
    #[serde(default)]
    pub severity: String,
}

/// One merged schedule entry (`GET /api/v1/schedule?from=&to=`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub show_id: Option<String>,
    pub caster_id: String,
    pub kind: String,
    pub title: String,
    pub starts_at: String,
    #[serde(default)]
    pub ends_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub persisted: bool,
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

/// Result of `POST /api/v1/schedule/expand`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandResult {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub skipped: i64,
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

/// Full broadcast (`broadcasts` row + API warnings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Broadcast {
    pub id: String,
    #[serde(default)]
    pub show_id: Option<String>,
    pub caster_id: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub starts_at: String,
    #[serde(default)]
    pub ends_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub tape_id: Option<String>,
    #[serde(default)]
    pub queue_id: Option<String>,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

/// Full show (`shows` row + parsed recurrence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Show {
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub links: Vec<serde_json::Value>,
    #[serde(default)]
    pub recurrence: Option<String>,
    #[serde(default)]
    pub caster_id: Option<String>,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Body of `POST /shows`.
#[derive(Debug, Clone, Default)]
pub struct NewShow {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub artwork_url: Option<String>,
    pub recurrence: Option<String>,
    pub caster_id: Option<String>,
    pub created_by: String,
}

/// Body of `PATCH /shows/{id}` (COALESCE patch, no `version` — schema).
#[derive(Debug, Clone, Default)]
pub struct UpdateShow {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub recurrence: Option<String>,
    pub caster_id: Option<String>,
}

/// Body of `POST /broadcasts`.
#[derive(Debug, Clone, Default)]
pub struct NewBroadcast {
    pub caster_id: String,
    pub show_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub tape_id: Option<String>,
    pub queue_id: Option<String>,
    pub created_by: Option<String>,
}

/// Body of `PATCH /broadcasts/{id}`.
///
/// The `Option<Option<String>>` fields mirror the scheduler's `double_option`:
/// outer `None` keeps the value, `Some(None)` clears it (JSON `null`),
/// `Some(Some(v))` replaces it.
#[derive(Debug, Clone, Default)]
pub struct UpdateBroadcast {
    pub kind: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<Option<String>>,
    pub show_id: Option<Option<String>>,
    pub tape_id: Option<Option<String>>,
    pub queue_id: Option<Option<String>>,
    pub version: i64,
}

/// Schedule/show/broadcast failure (mirrors the scheduler status mapping).
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    /// Unknown show/broadcast (scheduler `404`).
    #[error("not found")]
    NotFound,
    /// Hard conflict (scheduler `409`), e.g. `show_overlap`.
    #[error("conflict {code}: {message}")]
    Conflict { code: String, message: String },
    /// Field-level rejection (scheduler `422`).
    #[error("{message}")]
    Validation { field: String, message: String },
    /// Transport/parse failure or unexpected status.
    #[error("upstream error: {0}")]
    Upstream(String),
}

impl ScheduleError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, ScheduleError::NotFound)
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, ScheduleError::Conflict { .. })
    }

    /// The scheduler `code` of a `409` (e.g. `show_overlap`).
    pub fn conflict_code(&self) -> Option<&str> {
        match self {
            ScheduleError::Conflict { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// Schedule-window reads. `viewer` is the session `caster_id` (never client
/// input, §1) and is sent as `X-Caster-Id`.
#[async_trait]
pub trait ScheduleApi: Send + Sync {
    /// `GET /schedule?from=&to=` (RFC3339 window, scheduler expands RRULEs).
    async fn window(
        &self,
        viewer: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<Vec<ScheduleEntry>, ScheduleError>;

    /// `POST /schedule/expand?from=&to=` (idempotent materialisation).
    async fn expand(
        &self,
        viewer: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<ExpandResult, ScheduleError>;
}

/// Show CRUD. `viewer` is the session `caster_id`.
#[async_trait]
pub trait ShowApi: Send + Sync {
    async fn list(
        &self,
        viewer: Option<&str>,
        caster_id: Option<&str>,
    ) -> Result<Vec<Show>, ScheduleError>;
    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Show, ScheduleError>;
    async fn create(&self, viewer: Option<&str>, req: &NewShow) -> Result<Show, ScheduleError>;
    async fn update(
        &self,
        viewer: Option<&str>,
        id: &str,
        req: &UpdateShow,
    ) -> Result<Show, ScheduleError>;
}

/// Broadcast CRUD. `viewer` is the session `caster_id`.
#[async_trait]
pub trait BroadcastApi: Send + Sync {
    async fn create(
        &self,
        viewer: Option<&str>,
        req: &NewBroadcast,
    ) -> Result<Broadcast, ScheduleError>;
    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Broadcast, ScheduleError>;
    async fn update(
        &self,
        viewer: Option<&str>,
        id: &str,
        req: &UpdateBroadcast,
    ) -> Result<Broadcast, ScheduleError>;
    async fn delete(&self, viewer: Option<&str>, id: &str) -> Result<(), ScheduleError>;
}

/// Maps an error response to a [`ScheduleError`] (shared by the HTTP impls).
async fn schedule_error_from(response: reqwest::Response) -> ScheduleError {
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    let code = body["error"]["code"].as_str().unwrap_or("").to_string();
    let field = body["error"]["details"]["field"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("upstream error")
        .to_string();
    match status.as_u16() {
        404 => ScheduleError::NotFound,
        409 => ScheduleError::Conflict { code, message },
        422 => ScheduleError::Validation { field, message },
        other => ScheduleError::Upstream(format!("scheduler returned {other}: {message}")),
    }
}

/// HTTP client for `/api/v1/schedule*`.
pub struct HttpScheduleApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpScheduleApi {
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

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        viewer: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, format!("{}/api/v1{path}", self.base_url))
            .bearer_auth(&self.service_token);
        if let Some(viewer) = viewer {
            builder = builder.header("x-caster-id", viewer);
        }
        builder
    }
}

#[async_trait]
impl ScheduleApi for HttpScheduleApi {
    async fn window(
        &self,
        viewer: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<Vec<ScheduleEntry>, ScheduleError> {
        let response = self
            .request(reqwest::Method::GET, "/schedule", viewer)
            .query(&[("from", from), ("to", to)])
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn expand(
        &self,
        viewer: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<ExpandResult, ScheduleError> {
        let response = self
            .request(reqwest::Method::POST, "/schedule/expand", viewer)
            .query(&[("from", from), ("to", to)])
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }
}

/// HTTP client for `/api/v1/shows*`.
pub struct HttpShowApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpShowApi {
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

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        viewer: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, format!("{}/api/v1{path}", self.base_url))
            .bearer_auth(&self.service_token);
        if let Some(viewer) = viewer {
            builder = builder.header("x-caster-id", viewer);
        }
        builder
    }
}

#[async_trait]
impl ShowApi for HttpShowApi {
    async fn list(
        &self,
        viewer: Option<&str>,
        caster_id: Option<&str>,
    ) -> Result<Vec<Show>, ScheduleError> {
        let mut req = self.request(reqwest::Method::GET, "/shows", viewer);
        if let Some(caster_id) = caster_id {
            req = req.query(&[("caster_id", caster_id)]);
        }
        let response = req
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Show, ScheduleError> {
        let response = self
            .request(reqwest::Method::GET, &format!("/shows/{id}"), viewer)
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn create(&self, viewer: Option<&str>, req: &NewShow) -> Result<Show, ScheduleError> {
        let response = self
            .request(reqwest::Method::POST, "/shows", viewer)
            .json(&serde_json::json!({
                "slug": req.slug,
                "name": req.name,
                "description": req.description,
                "artwork_url": req.artwork_url,
                "recurrence": req.recurrence,
                "caster_id": req.caster_id,
                "created_by": req.created_by,
            }))
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn update(
        &self,
        viewer: Option<&str>,
        id: &str,
        req: &UpdateShow,
    ) -> Result<Show, ScheduleError> {
        let mut body = serde_json::Map::new();
        if let Some(v) = &req.slug {
            body.insert("slug".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.name {
            body.insert("name".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.description {
            body.insert("description".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.recurrence {
            body.insert("recurrence".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.caster_id {
            body.insert("caster_id".into(), serde_json::json!(v));
        }
        let response = self
            .request(reqwest::Method::PATCH, &format!("/shows/{id}"), viewer)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }
}

/// HTTP client for `/api/v1/broadcasts*`.
pub struct HttpBroadcastApi {
    client: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl HttpBroadcastApi {
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

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        viewer: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, format!("{}/api/v1{path}", self.base_url))
            .bearer_auth(&self.service_token);
        if let Some(viewer) = viewer {
            builder = builder.header("x-caster-id", viewer);
        }
        builder
    }
}

/// Inserts `key` only when the outer option is present; a `Some(None)` becomes
/// an explicit JSON `null` (clears the column), matching `double_option`.
fn insert_double(
    body: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &Option<Option<String>>,
) {
    match value {
        None => {}
        Some(Some(v)) => {
            body.insert(key.to_string(), serde_json::json!(v));
        }
        Some(None) => {
            body.insert(key.to_string(), serde_json::Value::Null);
        }
    }
}

#[async_trait]
impl BroadcastApi for HttpBroadcastApi {
    async fn create(
        &self,
        viewer: Option<&str>,
        req: &NewBroadcast,
    ) -> Result<Broadcast, ScheduleError> {
        let response = self
            .request(reqwest::Method::POST, "/broadcasts", viewer)
            .json(&serde_json::json!({
                "caster_id": req.caster_id,
                "show_id": req.show_id,
                "kind": req.kind,
                "title": req.title,
                "description": req.description,
                "starts_at": req.starts_at,
                "ends_at": req.ends_at,
                "tape_id": req.tape_id,
                "queue_id": req.queue_id,
                "created_by": req.created_by,
            }))
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Broadcast, ScheduleError> {
        let response = self
            .request(reqwest::Method::GET, &format!("/broadcasts/{id}"), viewer)
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn update(
        &self,
        viewer: Option<&str>,
        id: &str,
        req: &UpdateBroadcast,
    ) -> Result<Broadcast, ScheduleError> {
        let mut body = serde_json::Map::new();
        body.insert("version".into(), serde_json::json!(req.version));
        if let Some(v) = &req.kind {
            body.insert("kind".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.title {
            body.insert("title".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.description {
            body.insert("description".into(), serde_json::json!(v));
        }
        if let Some(v) = &req.starts_at {
            body.insert("starts_at".into(), serde_json::json!(v));
        }
        insert_double(&mut body, "ends_at", &req.ends_at);
        insert_double(&mut body, "show_id", &req.show_id);
        insert_double(&mut body, "tape_id", &req.tape_id);
        insert_double(&mut body, "queue_id", &req.queue_id);
        let response = self
            .request(reqwest::Method::PATCH, &format!("/broadcasts/{id}"), viewer)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if !response.status().is_success() {
            return Err(schedule_error_from(response).await);
        }
        response
            .json()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))
    }

    async fn delete(&self, viewer: Option<&str>, id: &str) -> Result<(), ScheduleError> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                &format!("/broadcasts/{id}"),
                viewer,
            )
            .send()
            .await
            .map_err(|e| ScheduleError::Upstream(e.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(schedule_error_from(response).await)
        }
    }
}

// ---------------------------------------------------------------------------
// In-memory fakes (shared store so shows/broadcasts/schedule agree)
// ---------------------------------------------------------------------------

fn t(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn intervals_overlap(
    a_start: DateTime<Utc>,
    a_end: Option<DateTime<Utc>>,
    b_start: DateTime<Utc>,
    b_end: Option<DateTime<Utc>>,
) -> bool {
    let a_before_b_end = b_end.is_none_or(|b_end| a_start < b_end);
    let b_before_a_end = a_end.is_none_or(|a_end| b_start < a_end);
    a_before_b_end && b_before_a_end
}

fn is_valid_recurrence(raw: &str) -> bool {
    let upper = raw.to_ascii_uppercase();
    upper.contains("DTSTART=") && upper.contains("FREQ=")
}

/// Validates the kind/content matrix (mirrors `scheduler` §3.3).
fn validate_broadcast_content(
    kind: &str,
    tape_id: Option<&str>,
    queue_id: Option<&str>,
) -> Result<(), ScheduleError> {
    let err = |field: &str, message: &str| ScheduleError::Validation {
        field: field.into(),
        message: message.into(),
    };
    match kind {
        "live" => {
            if tape_id.is_some() {
                return Err(err("tape_id", "kind 'live' must not reference a tape"));
            }
            if queue_id.is_some() {
                return Err(err("queue_id", "kind 'live' must not reference a queue"));
            }
        }
        "recording" => {
            if tape_id.is_none() {
                return Err(err("tape_id", "kind 'recording' requires a tape_id"));
            }
            if queue_id.is_some() {
                return Err(err(
                    "queue_id",
                    "kind 'recording' must not reference a queue",
                ));
            }
        }
        "playlist" => {
            if queue_id.is_none() {
                return Err(err("queue_id", "kind 'playlist' requires a queue_id"));
            }
            if tape_id.is_some() {
                return Err(err("tape_id", "kind 'playlist' must not reference a tape"));
            }
        }
        other => {
            return Err(err(
                "kind",
                &format!("unknown kind '{other}', expected 'live', 'recording' or 'playlist'"),
            ));
        }
    }
    Ok(())
}

fn warning_for(code: &str, message: String, other: &Broadcast) -> Warning {
    Warning {
        code: code.into(),
        message,
        broadcast_id: Some(other.id.clone()),
        show_id: other.show_id.clone(),
        caster_id: Some(other.caster_id.clone()),
        starts_at: Some(other.starts_at.clone()),
        ends_at: other.ends_at.clone(),
        severity: "warning".into(),
    }
}

#[derive(Default)]
struct FakeScheduleStore {
    shows: std::sync::Mutex<Vec<Show>>,
    broadcasts: std::sync::Mutex<Vec<Broadcast>>,
    counter: std::sync::Mutex<u64>,
}

impl FakeScheduleStore {
    fn next_id(&self, prefix: &str) -> String {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        format!("{prefix}-{counter}")
    }

    /// Soft warnings for `candidate` against every other broadcast (§4.2).
    fn warnings_for(&self, candidate: &Broadcast) -> Vec<Warning> {
        let Some(c_start) = t(&candidate.starts_at) else {
            return Vec::new();
        };
        let c_end = candidate.ends_at.as_deref().and_then(t);
        let all = self.broadcasts.lock().unwrap();
        let mut out = Vec::new();
        for other in all.iter() {
            if other.id == candidate.id || other.status == "cancelled" {
                continue;
            }
            let Some(o_start) = t(&other.starts_at) else {
                continue;
            };
            let o_end = other.ends_at.as_deref().and_then(t);
            if !intervals_overlap(c_start, c_end, o_start, o_end) {
                continue;
            }
            let same_show = candidate.show_id.is_some() && candidate.show_id == other.show_id;
            if same_show {
                continue; // hard conflict, never persisted.
            }
            out.push(warning_for(
                "overlap",
                format!("Überlappt mit Broadcast „{}“", other.title),
                other,
            ));
            if candidate.kind == "live" && other.kind == "live" {
                out.push(warning_for(
                    "live_overlap",
                    format!(
                        "Live-Überlappung mit „{}“: nur ein Broadcast kann live sein (FCFS)",
                        other.title
                    ),
                    other,
                ));
            }
            if other.caster_id == candidate.caster_id {
                out.push(warning_for(
                    "caster_overlap",
                    format!(
                        "Caster ist im selben Zeitraum bereits eingeplant („{}“)",
                        other.title
                    ),
                    other,
                ));
            }
        }
        out
    }

    /// `Some(conflicting_id)` when the same show overlaps itself (hard `409`).
    fn hard_overlap(&self, candidate: &Broadcast) -> Option<String> {
        let show_id = candidate.show_id.as_deref()?;
        let c_start = t(&candidate.starts_at)?;
        let c_end = candidate.ends_at.as_deref().and_then(t);
        let all = self.broadcasts.lock().unwrap();
        all.iter()
            .find(|other| {
                other.id != candidate.id
                    && other.status != "cancelled"
                    && other.show_id.as_deref() == Some(show_id)
                    && t(&other.starts_at).is_some_and(|o_start| {
                        let o_end = other.ends_at.as_deref().and_then(t);
                        intervals_overlap(c_start, c_end, o_start, o_end)
                    })
            })
            .map(|other| other.id.clone())
    }
}

/// A shared in-memory scheduler backend: one store behind the three fakes so
/// a created broadcast is visible in `window` and `get` (test harness).
#[derive(Clone, Default)]
pub struct FakeScheduleBackend {
    store: std::sync::Arc<FakeScheduleStore>,
}

impl FakeScheduleBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn schedule(&self) -> FakeScheduleApi {
        FakeScheduleApi {
            store: self.store.clone(),
        }
    }

    pub fn shows(&self) -> FakeShowApi {
        FakeShowApi {
            store: self.store.clone(),
        }
    }

    pub fn broadcasts(&self) -> FakeBroadcastApi {
        FakeBroadcastApi {
            store: self.store.clone(),
        }
    }

    pub fn seed_show(&self, show: Show) {
        self.store.shows.lock().unwrap().push(show);
    }

    pub fn seed_broadcast(&self, broadcast: Broadcast) {
        self.store.broadcasts.lock().unwrap().push(broadcast);
    }

    pub fn all_shows(&self) -> Vec<Show> {
        self.store.shows.lock().unwrap().clone()
    }

    pub fn all_broadcasts(&self) -> Vec<Broadcast> {
        self.store.broadcasts.lock().unwrap().clone()
    }
}

/// In-memory [`ScheduleApi`].
pub struct FakeScheduleApi {
    store: std::sync::Arc<FakeScheduleStore>,
}

#[async_trait]
impl ScheduleApi for FakeScheduleApi {
    async fn window(
        &self,
        _viewer: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<Vec<ScheduleEntry>, ScheduleError> {
        let from = t(from).ok_or_else(|| ScheduleError::Validation {
            field: "from".into(),
            message: "invalid from".into(),
        })?;
        let to = t(to).ok_or_else(|| ScheduleError::Validation {
            field: "to".into(),
            message: "invalid to".into(),
        })?;
        let broadcasts = self.store.broadcasts.lock().unwrap().clone();
        let mut entries = Vec::new();
        for b in broadcasts {
            if b.status == "cancelled" {
                continue;
            }
            let Some(start) = t(&b.starts_at) else {
                continue;
            };
            let end = b.ends_at.as_deref().and_then(t);
            if !intervals_overlap(start, end, from, Some(to)) {
                continue;
            }
            entries.push(ScheduleEntry {
                id: Some(b.id.clone()),
                show_id: b.show_id.clone(),
                caster_id: b.caster_id.clone(),
                kind: b.kind.clone(),
                title: b.title.clone(),
                starts_at: b.starts_at.clone(),
                ends_at: b.ends_at.clone(),
                status: b.status.clone(),
                persisted: true,
                warnings: self.store.warnings_for(&b),
            });
        }
        entries.sort_by(|a, b| a.starts_at.cmp(&b.starts_at).then(a.id.cmp(&b.id)));
        Ok(entries)
    }

    async fn expand(
        &self,
        _viewer: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<ExpandResult, ScheduleError> {
        Ok(ExpandResult {
            from: from.into(),
            to: to.into(),
            created: 0,
            skipped: 0,
            warnings: Vec::new(),
        })
    }
}

/// In-memory [`ShowApi`].
pub struct FakeShowApi {
    store: std::sync::Arc<FakeScheduleStore>,
}

#[async_trait]
impl ShowApi for FakeShowApi {
    async fn list(
        &self,
        _viewer: Option<&str>,
        caster_id: Option<&str>,
    ) -> Result<Vec<Show>, ScheduleError> {
        let shows = self.store.shows.lock().unwrap();
        Ok(shows
            .iter()
            .filter(|s| caster_id.is_none_or(|c| s.caster_id.as_deref() == Some(c)))
            .cloned()
            .collect())
    }

    async fn get(&self, _viewer: Option<&str>, id: &str) -> Result<Show, ScheduleError> {
        self.store
            .shows
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or(ScheduleError::NotFound)
    }

    async fn create(&self, _viewer: Option<&str>, req: &NewShow) -> Result<Show, ScheduleError> {
        if req.slug.trim().is_empty() {
            return Err(ScheduleError::Validation {
                field: "slug".into(),
                message: "slug must not be empty".into(),
            });
        }
        if req.name.trim().is_empty() {
            return Err(ScheduleError::Validation {
                field: "name".into(),
                message: "name must not be empty".into(),
            });
        }
        if let Some(raw) = &req.recurrence {
            if !is_valid_recurrence(raw) {
                return Err(ScheduleError::Validation {
                    field: "recurrence".into(),
                    message: "invalid recurrence rule".into(),
                });
            }
        }
        let show = Show {
            id: self.store.next_id("show"),
            slug: req.slug.trim().into(),
            name: req.name.trim().into(),
            description: req.description.clone().unwrap_or_default(),
            artwork_url: req.artwork_url.clone(),
            links: Vec::new(),
            recurrence: req.recurrence.clone(),
            caster_id: req.caster_id.clone(),
            created_by: req.created_by.clone(),
            created_at: FAKE_TS.into(),
            updated_at: FAKE_TS.into(),
        };
        self.store.shows.lock().unwrap().push(show.clone());
        Ok(show)
    }

    async fn update(
        &self,
        _viewer: Option<&str>,
        id: &str,
        req: &UpdateShow,
    ) -> Result<Show, ScheduleError> {
        if let Some(raw) = &req.recurrence {
            if !is_valid_recurrence(raw) {
                return Err(ScheduleError::Validation {
                    field: "recurrence".into(),
                    message: "invalid recurrence rule".into(),
                });
            }
        }
        let mut shows = self.store.shows.lock().unwrap();
        let show = shows
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(ScheduleError::NotFound)?;
        if let Some(v) = &req.slug {
            show.slug = v.clone();
        }
        if let Some(v) = &req.name {
            show.name = v.clone();
        }
        if let Some(v) = &req.description {
            show.description = v.clone();
        }
        if let Some(v) = &req.recurrence {
            show.recurrence = Some(v.clone());
        }
        if let Some(v) = &req.caster_id {
            show.caster_id = Some(v.clone());
        }
        show.updated_at = FAKE_TS.into();
        Ok(show.clone())
    }
}

/// In-memory [`BroadcastApi`].
pub struct FakeBroadcastApi {
    store: std::sync::Arc<FakeScheduleStore>,
}

#[async_trait]
impl BroadcastApi for FakeBroadcastApi {
    async fn create(
        &self,
        _viewer: Option<&str>,
        req: &NewBroadcast,
    ) -> Result<Broadcast, ScheduleError> {
        let starts = t(&req.starts_at).ok_or_else(|| ScheduleError::Validation {
            field: "starts_at".into(),
            message: "invalid starts_at".into(),
        })?;
        let ends = match &req.ends_at {
            Some(raw) => Some(t(raw).ok_or_else(|| ScheduleError::Validation {
                field: "ends_at".into(),
                message: "invalid ends_at".into(),
            })?),
            None => None,
        };
        if let Some(end) = ends {
            if end <= starts {
                return Err(ScheduleError::Validation {
                    field: "ends_at".into(),
                    message: "ends_at must be after starts_at".into(),
                });
            }
        }
        validate_broadcast_content(&req.kind, req.tape_id.as_deref(), req.queue_id.as_deref())?;
        let broadcast = Broadcast {
            id: self.store.next_id("bc"),
            show_id: req.show_id.clone(),
            caster_id: req.caster_id.clone(),
            kind: req.kind.clone(),
            title: req.title.clone(),
            description: req.description.clone().unwrap_or_default(),
            starts_at: req.starts_at.clone(),
            ends_at: req.ends_at.clone(),
            status: "scheduled".into(),
            tape_id: req.tape_id.clone(),
            queue_id: req.queue_id.clone(),
            version: 1,
            created_by: req
                .created_by
                .clone()
                .unwrap_or_else(|| req.caster_id.clone()),
            created_at: FAKE_TS.into(),
            updated_at: FAKE_TS.into(),
            warnings: Vec::new(),
        };
        if let Some(conflicting) = self.store.hard_overlap(&broadcast) {
            return Err(ScheduleError::Conflict {
                code: "show_overlap".into(),
                message: format!("show already has a broadcast ({conflicting}) in this slot"),
            });
        }
        self.store
            .broadcasts
            .lock()
            .unwrap()
            .push(broadcast.clone());
        Ok(self.decorate(broadcast))
    }

    async fn get(&self, _viewer: Option<&str>, id: &str) -> Result<Broadcast, ScheduleError> {
        let broadcast = self
            .store
            .broadcasts
            .lock()
            .unwrap()
            .iter()
            .find(|b| b.id == id)
            .cloned()
            .ok_or(ScheduleError::NotFound)?;
        Ok(self.decorate(broadcast))
    }

    async fn update(
        &self,
        _viewer: Option<&str>,
        id: &str,
        req: &UpdateBroadcast,
    ) -> Result<Broadcast, ScheduleError> {
        let mut broadcasts = self.store.broadcasts.lock().unwrap();
        let current = broadcasts
            .iter()
            .find(|b| b.id == id)
            .cloned()
            .ok_or(ScheduleError::NotFound)?;
        if current.version != req.version {
            return Err(ScheduleError::Conflict {
                code: "version_conflict".into(),
                message: "broadcast was changed elsewhere".into(),
            });
        }
        let kind = req.kind.clone().unwrap_or_else(|| current.kind.clone());
        let title = req.title.clone().unwrap_or_else(|| current.title.clone());
        let starts_at = req
            .starts_at
            .clone()
            .unwrap_or_else(|| current.starts_at.clone());
        let ends_at = match &req.ends_at {
            None => current.ends_at.clone(),
            Some(v) => v.clone(),
        };
        let show_id = match &req.show_id {
            None => current.show_id.clone(),
            Some(v) => v.clone(),
        };
        let tape_id = match &req.tape_id {
            None => current.tape_id.clone(),
            Some(v) => v.clone(),
        };
        let queue_id = match &req.queue_id {
            None => current.queue_id.clone(),
            Some(v) => v.clone(),
        };
        let starts = t(&starts_at).ok_or_else(|| ScheduleError::Validation {
            field: "starts_at".into(),
            message: "invalid starts_at".into(),
        })?;
        let ends = match &ends_at {
            Some(raw) => Some(t(raw).ok_or_else(|| ScheduleError::Validation {
                field: "ends_at".into(),
                message: "invalid ends_at".into(),
            })?),
            None => None,
        };
        if let Some(end) = ends {
            if end <= starts {
                return Err(ScheduleError::Validation {
                    field: "ends_at".into(),
                    message: "ends_at must be after starts_at".into(),
                });
            }
        }
        validate_broadcast_content(&kind, tape_id.as_deref(), queue_id.as_deref())?;
        let updated = Broadcast {
            id: current.id.clone(),
            show_id,
            caster_id: current.caster_id.clone(),
            kind,
            title,
            description: req
                .description
                .clone()
                .unwrap_or_else(|| current.description.clone()),
            starts_at,
            ends_at,
            status: current.status.clone(),
            tape_id,
            queue_id,
            version: current.version + 1,
            created_by: current.created_by.clone(),
            created_at: current.created_at.clone(),
            updated_at: FAKE_TS.into(),
            warnings: Vec::new(),
        };
        if let Some(conflicting) = self.store.hard_overlap(&updated) {
            return Err(ScheduleError::Conflict {
                code: "show_overlap".into(),
                message: format!("show already has a broadcast ({conflicting}) in this slot"),
            });
        }
        if let Some(slot) = broadcasts.iter_mut().find(|b| b.id == id) {
            *slot = updated.clone();
        }
        drop(broadcasts);
        Ok(self.decorate(updated))
    }

    async fn delete(&self, _viewer: Option<&str>, id: &str) -> Result<(), ScheduleError> {
        let mut broadcasts = self.store.broadcasts.lock().unwrap();
        let before = broadcasts.len();
        broadcasts.retain(|b| b.id != id);
        if broadcasts.len() == before {
            return Err(ScheduleError::NotFound);
        }
        Ok(())
    }
}

impl FakeBroadcastApi {
    fn decorate(&self, mut broadcast: Broadcast) -> Broadcast {
        broadcast.warnings = self.store.warnings_for(&broadcast);
        broadcast
    }
}
