//! Library handlers (Issue #19, US-4…US-7): search/list (+ fragment), track
//! detail, visibility toggle and delete.
//!
//! The portal is thin (§4.2): every read/write goes through the [`TrackApi`]
//! write-through with the session `caster_id` as the viewer context. The
//! scheduler already applies the §1 scope; [`crate::visibility`] repeats it as
//! defense-in-depth before rendering.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::api_client::{Track, TrackError, TrackQuery};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::rbac;
use crate::templates::{LibraryPage, LibraryResultsFragment, TrackDetailPage, TrackRowFragment};
use crate::visibility;

/// Page size for the library list.
const PAGE_SIZE: i64 = 20;

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some()
}

fn track_error(err: TrackError) -> PortalError {
    if err.is_not_found() {
        PortalError::NotFound
    } else {
        PortalError::Upstream(err.to_string())
    }
}

/// Query parameters of `GET /library`.
#[derive(Debug, Deserialize)]
pub struct LibraryQuery {
    pub q: Option<String>,
    pub visibility: Option<String>,
    pub owner: Option<String>,
    pub page: Option<i64>,
}

/// The normalised filter state shared by the page and its fragment.
struct FilterState {
    q: String,
    visibility: String,
    owner: String,
    page: i64,
    prev_page: Option<i64>,
    next_page: Option<i64>,
    tracks: Vec<Track>,
    can_delete: bool,
}

/// Resolves the query into page state, calling the scheduler within scope.
async fn resolve(
    state: &AppState,
    ctx: &AuthContext,
    params: &LibraryQuery,
) -> Result<FilterState, PortalError> {
    let q = params.q.clone().unwrap_or_default();
    let visibility = match params.visibility.as_deref() {
        Some("shared") => "shared".to_string(),
        Some("private") => "private".to_string(),
        _ => "all".to_string(),
    };
    let owner = params.owner.clone().unwrap_or_default();
    let page = params.page.unwrap_or(1).max(1);

    let query = TrackQuery {
        q: (!q.trim().is_empty()).then(|| q.clone()),
        visibility: (visibility != "all").then(|| visibility.clone()),
        owner_caster_id: (!owner.is_empty()).then(|| owner.clone()),
        // Fetch one extra row to detect a following page.
        limit: Some(PAGE_SIZE + 1),
        offset: Some((page - 1) * PAGE_SIZE),
    };

    let mut tracks = state
        .tracks
        .list(Some(&ctx.caster_id), &query)
        .await
        .map_err(track_error)?;
    // Defense-in-depth: never render a track that is not visible (§1).
    tracks.retain(|track| visibility::visible_to(track, ctx));

    let has_next = tracks.len() as i64 > PAGE_SIZE;
    tracks.truncate(PAGE_SIZE as usize);

    Ok(FilterState {
        q,
        visibility,
        owner,
        page,
        prev_page: (page > 1).then_some(page - 1),
        next_page: has_next.then_some(page + 1),
        can_delete: ctx.has("library.delete"),
        tracks,
    })
}

/// `GET /library` — full page, or the `library-results` fragment on `HX-Request`.
pub async fn library_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Query(params): Query<LibraryQuery>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.read")?;
    let f = resolve(&state, &ctx, &params).await?;

    if is_htmx(&headers) {
        return render(&LibraryResultsFragment {
            csrf_token: ctx.csrf_token.clone(),
            tracks: f.tracks,
            q: f.q,
            visibility: f.visibility,
            owner: f.owner,
            can_delete: f.can_delete,
            page: f.page,
            prev_page: f.prev_page,
            next_page: f.next_page,
        });
    }

    let jobs = state.upload_jobs.list_jobs(50).await.unwrap_or_default();
    render(&LibraryPage {
        csrf_token: ctx.csrf_token.clone(),
        jobs,
        tracks: f.tracks,
        q: f.q,
        visibility: f.visibility,
        owner: f.owner,
        can_delete: f.can_delete,
        page: f.page,
        prev_page: f.prev_page,
        next_page: f.next_page,
    })
}

/// `GET /library/tracks/{id}` — track detail (waveform, metadata, actions).
pub async fn track_detail(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.read")?;
    let track = state
        .tracks
        .get(Some(&ctx.caster_id), &id)
        .await
        .map_err(track_error)?;
    // Defense-in-depth: a foreign private track is a 404, never a 403.
    if !visibility::visible_to(&track, &ctx) {
        return Err(PortalError::NotFound);
    }
    render(&TrackDetailPage {
        csrf_token: ctx.csrf_token.clone(),
        track,
        can_delete: ctx.has("library.delete"),
    })
}

/// Body of `POST /library/tracks/{id}/visibility`.
#[derive(Debug, Deserialize)]
pub struct VisibilityForm {
    pub visibility: String,
}

/// `POST /library/tracks/{id}/visibility` — switch shared/private (owner only).
pub async fn track_visibility(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<VisibilityForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.upload")?;
    if form.visibility != "shared" && form.visibility != "private" {
        return Err(PortalError::BadRequest(
            "visibility must be 'shared' or 'private'".into(),
        ));
    }
    let track = state
        .tracks
        .set_visibility(Some(&ctx.caster_id), &id, &form.visibility)
        .await
        .map_err(track_error)?;

    if is_htmx(&headers) {
        return render(&TrackRowFragment {
            csrf_token: ctx.csrf_token.clone(),
            track,
            can_delete: ctx.has("library.delete"),
        });
    }
    Ok(redirect(&format!("/library/tracks/{id}")))
}

/// `POST /library/tracks/{id}/delete` — delete a track.
pub async fn track_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "library.delete")?;
    state
        .tracks
        .delete(Some(&ctx.caster_id), &id)
        .await
        .map_err(track_error)?;

    if is_htmx(&headers) {
        let mut response = StatusCode::OK.into_response();
        response.headers_mut().insert(
            header::HeaderName::from_static("hx-redirect"),
            header::HeaderValue::from_static("/library"),
        );
        return Ok(response);
    }
    Ok(redirect("/library"))
}
