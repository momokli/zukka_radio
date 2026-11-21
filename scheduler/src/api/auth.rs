//! Auth layer (Issues #13 + #16).
//!
//! Two distinct concerns live here:
//!
//! * The **Liquidsoap token** layer ([`require_token`]) guards only the
//!   `/playback/*` sub-router (wired in [`crate::api::playback`]). Fail-closed.
//! * The **identity authority** (Issue #16): service-token-guarded `/internal`
//!   handlers for `verify`, session write-through and password changes. The
//!   scheduler is the only writer of `casters`/`sessions`/`roles` (§4.1/§4.3).

use std::sync::OnceLock;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sqlx::SqlitePool;

use crate::api::ApiJson;
use crate::dto::{
    CasterDto, ChangePasswordRequest, CreateCasterRequest, CreateSessionRequest,
    CreatedCasterResponse, CreatedSessionDto, HarborAuthRequest, HarborAuthResponse,
    SessionValidationDto, StreamPasswordRequest, StreamPasswordResponse, UpdateCasterRequest,
    VerifyRequest, VerifyResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::rate_limit::WindowLimiter;
use crate::store;

/// Header carrying the presented token (§4.1).
pub const TOKEN_HEADER: &str = "x-liquidsoap-token";

/// Environment variable holding the configured token.
pub const TOKEN_ENV: &str = "LIQUIDSOAP_TOKEN";

/// Environment variable holding the portal↔scheduler service token (§4.1).
pub const SERVICE_TOKEN_ENV: &str = "SCHEDULER_SERVICE_TOKEN";

/// Header carrying the raw browser session token on the read route.
pub const SESSION_TOKEN_HEADER: &str = "x-session-token";

/// Fail-closed login rate limit: 5 failures / 15 min (§portal.md D3).
pub const LOGIN_MAX_ATTEMPTS: usize = 5;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Fail-closed token comparison (§2.1).
///
/// Returns `true` only when a non-empty `configured` token is present **and**
/// `presented` equals it exactly (constant-time). Every other combination
/// (env unset, env empty, header missing, mismatch) yields `false`.
pub fn check_token(configured: Option<&str>, presented: Option<&str>) -> bool {
    match configured {
        Some(cfg) if !cfg.is_empty() => match presented {
            Some(p) => ct_eq(cfg.as_bytes(), p.as_bytes()),
            None => false,
        },
        _ => false,
    }
}

/// Constant-time byte comparison (manual XOR fold, no extra dependency).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Middleware rejecting unauthenticated `/playback/*` requests with `401`.
///
/// The configured token is read **at request time** so a router built once in
/// tests picks up the test token without changing the router signature.
pub async fn require_token(req: Request, next: Next) -> Result<Response, ApiError> {
    let configured = std::env::var(TOKEN_ENV).ok();
    let presented = req
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if !check_token(configured.as_deref(), presented.as_deref()) {
        return Err(ApiError::Unauthorized);
    }
    Ok(next.run(req).await)
}

/// Middleware requiring `Authorization: Bearer $SCHEDULER_SERVICE_TOKEN` on
/// every `/internal/*` route (§4.1). Read at request time; fail-closed.
pub async fn require_service_token(req: Request, next: Next) -> Result<Response, ApiError> {
    let configured = std::env::var(SERVICE_TOKEN_ENV).ok();
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    if !check_token(configured.as_deref(), presented.as_deref()) {
        return Err(ApiError::Unauthorized);
    }
    Ok(next.run(req).await)
}

fn login_limiter() -> &'static WindowLimiter {
    static LIMITER: OnceLock<WindowLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| WindowLimiter::new(LOGIN_MAX_ATTEMPTS, LOGIN_WINDOW))
}

/// Clears the process-global login limiter (test helper).
pub fn reset_login_limiter() {
    login_limiter().clear();
}

/// Rate-limit key: `username` (case-insensitive) + client IP.
fn login_key(username: &str, headers: &HeaderMap) -> String {
    let ip = headers
        .get("x-forwarded-for")
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    format!("{}|{}", username.to_lowercase(), ip)
}

/// `POST /internal/auth/verify` — verify a login (§4.2).
pub async fn verify(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<VerifyRequest>,
) -> ApiResult<Json<VerifyResponse>> {
    let limiter = login_limiter();
    let key = login_key(&req.username, &headers);
    if limiter.is_blocked(&key) {
        return Err(ApiError::TooManyRequests);
    }
    match store::auth::verify_login(&pool, &req.username, &req.password).await {
        Ok(caster) => {
            limiter.reset(&key);
            Ok(Json(VerifyResponse { caster }))
        }
        Err(ApiError::Unauthorized) => {
            limiter.record_failure(&key);
            Err(ApiError::Unauthorized)
        }
        Err(err) => Err(err),
    }
}

/// `POST /internal/harbor/auth` — fail-closed Harbor ingest check (§4.2, #33).
///
/// Called by Liquidsoap's `input.harbor(auth=…)` callback on connection setup.
/// Must answer fast (< 200 ms): the store does one indexed lookup and exactly
/// one Argon2 verification, no extra DB roundtrip.
pub async fn harbor_auth(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<HarborAuthRequest>,
) -> ApiResult<Json<HarborAuthResponse>> {
    let auth =
        store::auth::authenticate_harbor(&pool, &req.mount, &req.username, &req.password).await?;
    Ok(Json(HarborAuthResponse {
        caster_id: auth.caster_id,
        display_name: auth.display_name,
        mount: auth.mount,
    }))
}

/// `POST /internal/sessions` — create a session (portal login).
pub async fn create_session(
    State(pool): State<SqlitePool>,
    ApiJson(req): ApiJson<CreateSessionRequest>,
) -> ApiResult<(StatusCode, Json<CreatedSessionDto>)> {
    let created = store::auth::create_session(&pool, &req).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `GET /internal/sessions/current` — validate the session identified by the
/// `X-Session-Token` header. The portal holds no authoritative session state;
/// it re-reads `{caster, role, permissions, csrf_token}` from here.
pub async fn current_session(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
) -> ApiResult<Json<SessionValidationDto>> {
    let token = headers
        .get(SESSION_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    match store::auth::validate_session(&pool, token).await? {
        Some(validation) => Ok(Json(validation)),
        None => Err(ApiError::Unauthorized),
    }
}

/// `PATCH /internal/sessions/{id}` — slide the idle window (batched ≤ 1×/min).
pub async fn touch_session(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if store::auth::touch_session(&pool, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("session", id))
    }
}

/// `DELETE /internal/sessions/{id}` — revoke a session (logout).
pub async fn revoke_session(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if store::auth::revoke_session(&pool, &id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("session", id))
    }
}

/// `GET /internal/casters/{id}` — caster lookup for the portal.
pub async fn get_caster(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> ApiResult<Json<CasterDto>> {
    Ok(Json(store::auth::get_caster(&pool, &id).await?))
}

/// `GET /internal/casters` — caster list for the admin page (Issue #101).
pub async fn list_casters(State(pool): State<SqlitePool>) -> ApiResult<Json<Vec<CasterDto>>> {
    Ok(Json(store::auth::list_casters(&pool).await?))
}

/// `POST /internal/casters` — create a caster (Issue #101). The generated
/// stream password is returned exactly once.
pub async fn create_caster(
    State(pool): State<SqlitePool>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<CreateCasterRequest>,
) -> ApiResult<(StatusCode, Json<CreatedCasterResponse>)> {
    let new_caster = store::auth::NewCaster {
        username: &req.username,
        display_name: &req.display_name,
        password: &req.password,
        role: &req.role,
        harbor_mount: non_empty(req.harbor_mount.as_deref()),
        email: non_empty(req.email.as_deref()),
        stream_username: non_empty(req.stream_username.as_deref()),
    };
    let actor = crate::api::viewer_caster_id(&headers);
    let created = store::auth::create_caster(&pool, &new_caster, actor.as_deref()).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedCasterResponse {
            caster: created.caster,
            stream_password: created.stream_password,
        }),
    ))
}

/// `PATCH /internal/casters/{id}` — update display name, email, role or
/// active flag (Issue #101).
pub async fn update_caster(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<UpdateCasterRequest>,
) -> ApiResult<Json<CasterDto>> {
    let patch = store::auth::CasterPatch {
        display_name: req.display_name.as_deref(),
        email: req.email.as_deref(),
        role: req.role.as_deref().filter(|value| !value.trim().is_empty()),
        is_active: req.is_active,
    };
    let actor = crate::api::viewer_caster_id(&headers);
    Ok(Json(
        store::auth::update_caster(&pool, &id, &patch, actor.as_deref()).await?,
    ))
}

/// `None` for absent or blank optional strings.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// `POST /internal/casters/{id}/password` — set the login password (rotation).
pub async fn change_password(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<ChangePasswordRequest>,
) -> ApiResult<StatusCode> {
    store::auth::change_password(&pool, &id, &req.password, req.keep_session_id.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /internal/casters/{id}/stream-password` — set or generate the stream
/// password. The generated value is returned exactly once.
pub async fn change_stream_password(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<StreamPasswordRequest>,
) -> ApiResult<Json<StreamPasswordResponse>> {
    let generated =
        store::auth::change_stream_password(&pool, &id, req.password.as_deref()).await?;
    Ok(Json(StreamPasswordResponse {
        password: generated,
    }))
}

#[cfg(test)]
mod tests {
    use super::check_token;

    #[test]
    fn fail_closed_matrix() {
        // env unset → always false
        assert!(!check_token(None, None));
        assert!(!check_token(None, Some("x")));
        // env empty → always false
        assert!(!check_token(Some(""), None));
        assert!(!check_token(Some(""), Some("")));
        assert!(!check_token(Some(""), Some("token")));
        // configured, header missing → false
        assert!(!check_token(Some("secret"), None));
        // configured, mismatch → false
        assert!(!check_token(Some("secret"), Some("other")));
        assert!(!check_token(Some("secret"), Some("secre")));
        // configured, exact match → true
        assert!(check_token(Some("secret"), Some("secret")));
    }
}
