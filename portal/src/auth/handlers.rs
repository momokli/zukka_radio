//! Login / logout / account handlers.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::api_client::IdentityError;
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::auth::csrf;
use crate::auth::session;
use crate::error::{PortalError, redirect};
use crate::templates::{AccountPage, DashboardPage, LiveBadgeView, LoginPage};

/// `POST /login` body.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    pub csrf_token: String,
}

/// `POST /account/password` body.
#[derive(Debug, Deserialize)]
pub struct PasswordForm {
    pub current_password: String,
    pub new_password: String,
    pub csrf_token: String,
}

/// `POST /account/stream-password` body.
#[derive(Debug, Deserialize)]
pub struct StreamForm {
    #[serde(default)]
    pub password: Option<String>,
    pub csrf_token: String,
}

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

fn render_with_status<T: Template>(status: StatusCode, page: &T) -> Result<Response, PortalError> {
    Ok((status, Html(page.render()?)).into_response())
}

fn cookie_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
}

fn client_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        })
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Rate-limit key: username (case-insensitive) + client IP.
fn login_key(username: &str, headers: &HeaderMap) -> String {
    format!(
        "{}|{}",
        username.to_lowercase(),
        client_ip(headers).unwrap_or_else(|| "unknown".into())
    )
}

fn login_csrf_cookie(token: &str) -> HeaderValue {
    HeaderValue::from_str(
        &format!(
            "{}=$token; HttpOnly; Secure; SameSite=Lax; Path=/",
            csrf::LOGIN_CSRF_COOKIE
        )
        .replace("$token", token),
    )
    .expect("valid cookie value")
}

fn upstream(err: IdentityError) -> PortalError {
    PortalError::Upstream(err.to_string())
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

/// `GET /login` — renders the form and (re)issues the double-submit CSRF cookie.
pub async fn login_get(
    State(_state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, PortalError> {
    let existing = session::parse_cookie(cookie_header(&headers), csrf::LOGIN_CSRF_COOKIE);
    let (token, issue) = match existing {
        Some(token) => (token, false),
        None => (csrf::random_token(), true),
    };
    let page = LoginPage {
        csrf_token: token.clone(),
        username: String::new(),
        error: None,
    };
    let mut response = render(&page)?;
    if issue {
        response
            .headers_mut()
            .append(header::SET_COOKIE, login_csrf_cookie(&token));
    }
    Ok(response)
}

/// `POST /login` — rate-limited verify → session → cookie → `303 /`.
pub async fn login_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, PortalError> {
    if !csrf::origin_ok(&headers, state.config.origin()) {
        return Err(PortalError::Forbidden("cross-origin login".into()));
    }

    // Double-submit: cookie value must equal the submitted form token.
    let cookie = session::parse_cookie(cookie_header(&headers), csrf::LOGIN_CSRF_COOKIE);
    let ok = cookie
        .as_deref()
        .map(|value| csrf::tokens_match(value, &form.csrf_token))
        .unwrap_or(false);
    if !ok {
        return Err(PortalError::Forbidden("invalid CSRF token".into()));
    }

    let key = login_key(&form.username, &headers);
    if state.limiter.is_blocked(&key) {
        return Err(PortalError::TooManyRequests);
    }

    match state.identity.verify(&form.username, &form.password).await {
        Ok(caster) => {
            state.limiter.reset(&key);
            let ip = client_ip(&headers);
            let ua = user_agent(&headers);
            let created = state
                .identity
                .create_session(&caster.id, ip.as_deref(), ua.as_deref())
                .await
                .map_err(upstream)?;
            let max_age = state.config.session_idle_days * 24 * 3600;
            let mut response = redirect("/");
            response.headers_mut().append(
                header::SET_COOKIE,
                session::set_cookie(&created.token, max_age),
            );
            Ok(response)
        }
        Err(IdentityError::InvalidCredentials) | Err(IdentityError::NotFound) => {
            state.limiter.record_failure(&key);
            // Identical message and timing for unknown user and wrong password.
            let page = LoginPage {
                csrf_token: form.csrf_token.clone(),
                username: form.username.clone(),
                error: Some("Benutzername oder Passwort ist falsch.".into()),
            };
            render_with_status(StatusCode::UNAUTHORIZED, &page)
        }
        Err(err) => Err(upstream(err)),
    }
}

/// `POST /logout` — revoke the session and clear the cookie.
pub async fn logout(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    if let Err(err) = state.identity.revoke_session(&ctx.session_id).await {
        tracing::warn!("failed to revoke session on logout: {err}");
    }
    let mut response = redirect("/login");
    response
        .headers_mut()
        .append(header::SET_COOKIE, session::clear_cookie());
    Ok(response)
}

// ---------------------------------------------------------------------------
// Dashboard / account
// ---------------------------------------------------------------------------

/// `GET /` — dashboard.
pub async fn dashboard(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    // D5: the badge reflects only the viewer's own live status.
    let is_live = match state.control.state(Some(&ctx.caster_id)).await {
        Ok(snapshot) => snapshot.live.active_caster_id.as_deref() == Some(ctx.caster_id.as_str()),
        Err(_) => false,
    };
    let page = DashboardPage {
        csrf_token: ctx.csrf_token.clone(),
        display_name: ctx.display_name.clone(),
        role: ctx.role.clone(),
        caster_id: ctx.caster_id.clone(),
        badge: LiveBadgeView { is_live },
    };
    render(&page)
}

async fn build_account_page(
    state: &AppState,
    ctx: &AuthContext,
    stream_password: Option<String>,
    notice: Option<String>,
    error: Option<String>,
) -> Result<AccountPage, PortalError> {
    let caster = state
        .identity
        .get_caster(&ctx.caster_id)
        .await
        .map_err(upstream)?;
    Ok(AccountPage {
        csrf_token: ctx.csrf_token.clone(),
        username: caster.username,
        display_name: caster.display_name,
        role: caster.role,
        harbor_mount: caster.harbor_mount,
        stream_username: caster.stream_username,
        stream_password,
        notice,
        error,
    })
}

/// `GET /account`.
pub async fn account_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    let page = build_account_page(&state, &ctx, None, None, None).await?;
    render(&page)
}

/// `POST /account/password` — verifies the current password, then rotates.
pub async fn account_password(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Form(form): Form<PasswordForm>,
) -> Result<Response, PortalError> {
    match state
        .identity
        .verify(&ctx.username, &form.current_password)
        .await
    {
        Ok(_) => {}
        Err(IdentityError::InvalidCredentials) | Err(IdentityError::NotFound) => {
            let page = build_account_page(
                &state,
                &ctx,
                None,
                None,
                Some("Aktuelles Passwort ist falsch.".into()),
            )
            .await?;
            return render_with_status(StatusCode::UNPROCESSABLE_ENTITY, &page);
        }
        Err(err) => return Err(upstream(err)),
    }

    match state
        .identity
        .change_password(&ctx.caster_id, &form.new_password, Some(&ctx.session_id))
        .await
    {
        Ok(()) => Ok(redirect("/account")),
        Err(IdentityError::Validation { message, .. }) => {
            let page = build_account_page(&state, &ctx, None, None, Some(message)).await?;
            render_with_status(StatusCode::UNPROCESSABLE_ENTITY, &page)
        }
        Err(err) => Err(upstream(err)),
    }
}

/// `POST /account/stream-password` — sets or generates the stream password.
pub async fn account_stream_password(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Form(form): Form<StreamForm>,
) -> Result<Response, PortalError> {
    let provided = form.password.as_deref().filter(|value| !value.is_empty());
    match state
        .identity
        .change_stream_password(&ctx.caster_id, provided)
        .await
    {
        Ok(generated) => {
            let notice = if generated.is_some() {
                "Neues Stream-Passwort generiert — jetzt kopieren.".to_string()
            } else {
                "Stream-Passwort aktualisiert.".to_string()
            };
            let page = build_account_page(&state, &ctx, generated, Some(notice), None).await?;
            render(&page)
        }
        Err(IdentityError::Validation { message, .. }) => {
            let page = build_account_page(&state, &ctx, None, None, Some(message)).await?;
            render_with_status(StatusCode::UNPROCESSABLE_ENTITY, &page)
        }
        Err(err) => Err(upstream(err)),
    }
}
