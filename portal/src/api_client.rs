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
