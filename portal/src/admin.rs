//! Admin user management (`/admin/users`, Issue #101).
//!
//! The portal checks the `caster.manage` permission (RBAC stays a portal
//! concern, §4.2); the scheduler is the only writer and enforces validation
//! and uniqueness. Every handler passes the acting caster as `X-Caster-Id` so
//! `event_log` records the actor.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::api_client::{CasterDto, CasterUpdate, IdentityError, NewCaster};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::rbac;
use crate::templates::{
    AdminUserFormValues, AdminUserRow, AdminUsersPage, CreatedCasterView, FieldErrorView,
    SelectOption,
};

/// The roles a caster can be assigned (§6.10.1).
const ROLES: [&str; 3] = ["admin", "operator", "caster"];

/// Body of `POST /admin/users`.
#[derive(Debug, Deserialize)]
pub struct CreateCasterForm {
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email: String,
    pub role: String,
    #[serde(default)]
    pub harbor_mount: String,
    pub password: String,
    pub csrf_token: String,
}

/// Body of `POST /admin/users/{id}`.
#[derive(Debug, Deserialize)]
pub struct UpdateCasterForm {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email: String,
    pub role: String,
    pub is_active: String,
    pub csrf_token: String,
}

/// Body of `POST /admin/users/{id}/stream-password`.
#[derive(Debug, Deserialize)]
pub struct StreamPasswordForm {
    #[serde(default)]
    pub password: Option<String>,
    pub csrf_token: String,
}

fn upstream(err: IdentityError) -> PortalError {
    PortalError::Upstream(err.to_string())
}

fn trim_opt(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_active(raw: &str) -> bool {
    matches!(raw.trim(), "true" | "1" | "on" | "yes")
}

/// Loads the caster list and renders the admin page with `status`.
#[allow(clippy::too_many_arguments)]
async fn render_page(
    status: StatusCode,
    state: &AppState,
    ctx: &AuthContext,
    created: Option<CreatedCasterView>,
    notice: Option<String>,
    error: Option<FieldErrorView>,
    form: AdminUserFormValues,
) -> Result<Response, PortalError> {
    let casters: Vec<CasterDto> = state.identity.list_casters().await.map_err(upstream)?;
    let active_admins = casters
        .iter()
        .filter(|caster| caster.role == "admin" && caster.is_active)
        .count();

    let rows = casters
        .into_iter()
        .map(|caster| {
            let role_options = ROLES
                .iter()
                .map(|role| SelectOption {
                    value: (*role).to_string(),
                    selected: *role == caster.role.as_str(),
                })
                .collect();
            AdminUserRow {
                is_self: caster.id == ctx.caster_id,
                is_last_admin: caster.role == "admin" && caster.is_active && active_admins <= 1,
                role_options,
                caster,
            }
        })
        .collect();

    let page = AdminUsersPage {
        csrf_token: ctx.csrf_token.clone(),
        casters: rows,
        roles: ROLES.iter().map(|role| (*role).to_string()).collect(),
        created,
        notice,
        error,
        form,
    };
    Ok((status, Html(page.render()?)).into_response())
}

/// `GET /admin/users` — list casters and the create form.
pub async fn admin_users_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "caster.manage")?;
    render_page(
        StatusCode::OK,
        &state,
        &ctx,
        None,
        None,
        None,
        AdminUserFormValues::default(),
    )
    .await
}

/// `POST /admin/users` — create a caster. The generated stream password is
/// rendered exactly once (no redirect, so it never travels through a URL).
pub async fn admin_user_create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Form(form): Form<CreateCasterForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "caster.manage")?;

    let values = AdminUserFormValues {
        username: form.username.clone(),
        display_name: form.display_name.clone(),
        email: form.email.clone(),
        role: form.role.clone(),
        harbor_mount: form.harbor_mount.clone(),
    };
    let new_caster = NewCaster {
        username: form.username.clone(),
        display_name: form.display_name.clone(),
        password: form.password.clone(),
        role: form.role.clone(),
        harbor_mount: trim_opt(&form.harbor_mount),
        email: trim_opt(&form.email),
        stream_username: None,
    };

    match state
        .identity
        .create_caster(Some(&ctx.caster_id), &new_caster)
        .await
    {
        Ok(created) => {
            let view = CreatedCasterView {
                username: created.caster.username,
                role: created.caster.role,
                harbor_mount: created.caster.harbor_mount,
                stream_password: created.stream_password,
            };
            render_page(
                StatusCode::CREATED,
                &state,
                &ctx,
                Some(view),
                None,
                None,
                AdminUserFormValues::default(),
            )
            .await
        }
        Err(IdentityError::Validation { field, message }) => {
            render_page(
                StatusCode::UNPROCESSABLE_ENTITY,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView { field, message }),
                values,
            )
            .await
        }
        Err(IdentityError::Conflict(message)) => {
            render_page(
                StatusCode::CONFLICT,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView {
                    field: "form".to_string(),
                    message: format!("{message} (Username, E-Mail oder Mount bereits vergeben)"),
                }),
                values,
            )
            .await
        }
        Err(err) => Err(upstream(err)),
    }
}

/// `POST /admin/users/{id}` — update display name, email, role and active flag.
pub async fn admin_user_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Form(form): Form<UpdateCasterForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "caster.manage")?;

    let is_active = parse_active(&form.is_active);
    let role = form.role.trim().to_string();
    let values = AdminUserFormValues {
        display_name: form.display_name.clone(),
        email: form.email.clone(),
        role: role.clone(),
        ..AdminUserFormValues::default()
    };

    // Never lock or demote your own account.
    if id == ctx.caster_id {
        if !is_active {
            return render_page(
                StatusCode::UNPROCESSABLE_ENTITY,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView {
                    field: "form".to_string(),
                    message: "Das eigene Konto kann nicht gesperrt werden.".to_string(),
                }),
                values,
            )
            .await;
        }
        if ctx.role == "admin" && role != "admin" {
            return render_page(
                StatusCode::UNPROCESSABLE_ENTITY,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView {
                    field: "form".to_string(),
                    message: "Das eigene Admin-Konto kann nicht degradiert werden.".to_string(),
                }),
                values,
            )
            .await;
        }
    }

    // Never remove the last active admin.
    let casters = state.identity.list_casters().await.map_err(upstream)?;
    let active_admins = casters
        .iter()
        .filter(|caster| caster.role == "admin" && caster.is_active)
        .count();
    if let Some(target) = casters.iter().find(|caster| caster.id == id) {
        let removes_admin = target.role == "admin" && target.is_active;
        if removes_admin && (role != "admin" || !is_active) && active_admins <= 1 {
            return render_page(
                StatusCode::UNPROCESSABLE_ENTITY,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView {
                    field: "form".to_string(),
                    message: "Der letzte aktive Admin kann nicht degradiert oder gesperrt werden."
                        .to_string(),
                }),
                values,
            )
            .await;
        }
    }

    let patch = CasterUpdate {
        display_name: Some(form.display_name.clone()),
        email: Some(form.email.clone()),
        role: Some(role),
        is_active: Some(is_active),
    };
    match state
        .identity
        .update_caster(Some(&ctx.caster_id), &id, &patch)
        .await
    {
        Ok(_) => Ok(redirect("/admin/users")),
        Err(IdentityError::Validation { field, message }) => {
            render_page(
                StatusCode::UNPROCESSABLE_ENTITY,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView { field, message }),
                values,
            )
            .await
        }
        Err(IdentityError::Conflict(message)) => {
            render_page(
                StatusCode::CONFLICT,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView {
                    field: "form".to_string(),
                    message,
                }),
                values,
            )
            .await
        }
        Err(IdentityError::NotFound) => Err(PortalError::NotFound),
        Err(err) => Err(upstream(err)),
    }
}

/// `POST /admin/users/{id}/stream-password` — generate (empty field) or set a
/// stream password. A generated value is shown exactly once.
pub async fn admin_user_stream_password(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Form(form): Form<StreamPasswordForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "caster.manage")?;

    let provided = form
        .password
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match state.identity.change_stream_password(&id, provided).await {
        Ok(Some(password)) => {
            let caster = state.identity.get_caster(&id).await.map_err(upstream)?;
            let view = CreatedCasterView {
                username: caster.username,
                role: caster.role,
                harbor_mount: caster.harbor_mount,
                stream_password: Some(password),
            };
            render_page(
                StatusCode::OK,
                &state,
                &ctx,
                Some(view),
                None,
                None,
                AdminUserFormValues::default(),
            )
            .await
        }
        Ok(None) => {
            render_page(
                StatusCode::OK,
                &state,
                &ctx,
                None,
                Some("Stream-Passwort aktualisiert.".to_string()),
                None,
                AdminUserFormValues::default(),
            )
            .await
        }
        Err(IdentityError::Validation { field, message }) => {
            render_page(
                StatusCode::UNPROCESSABLE_ENTITY,
                &state,
                &ctx,
                None,
                None,
                Some(FieldErrorView { field, message }),
                AdminUserFormValues::default(),
            )
            .await
        }
        Err(IdentityError::NotFound) => Err(PortalError::NotFound),
        Err(err) => Err(upstream(err)),
    }
}
