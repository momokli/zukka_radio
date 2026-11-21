//! End-to-end tests for the schedule/calendar UI (Issue #21) through the real
//! portal router.
//!
//! The scheduler is replaced by [`FakeScheduleBackend`] (which enforces the
//! same invariants and soft-warning rules as the real scheduler); login uses
//! the real session cookie + CSRF flow (mirrors `queue_ui_e2e.rs`). Explicit
//! proofs:
//!   (a) the calendar renders in Europe/Berlin and marks an open slot,
//!   (b) a soft overlap renders a warning but never blocks the write (D12),
//!   (c) a hard self-overlap of the same show is a `409` → toast + re-render,
//!   (d) `end < start` and `playlist` without `queue_id` are inline `422`s.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use zukka_portal::api_client::{
    Broadcast, CasterDto, CreatedSessionDto, IdentityApi, IdentityError, SessionValidationDto, Show,
};
use zukka_portal::api_client::{CasterUpdate, CreatedCaster, NewCaster};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

// ---------------------------------------------------------------------------
// Fake identity (one caster whose permission map is configurable)
// ---------------------------------------------------------------------------

struct FakeIdentity {
    dto: CasterDto,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new(perms: BTreeMap<String, bool>) -> Self {
        Self {
            dto: CasterDto {
                id: "c-1".into(),
                username: "mira".into(),
                display_name: "Mira".into(),
                email: None,
                role: "caster".into(),
                is_active: true,
                harbor_mount: None,
                stream_username: None,
                permissions: perms,
            },
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn list_casters(&self) -> Result<Vec<CasterDto>, IdentityError> {
        Ok(Vec::new())
    }

    async fn create_caster(
        &self,
        _actor: Option<&str>,
        _req: &NewCaster,
    ) -> Result<CreatedCaster, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }

    async fn update_caster(
        &self,
        _actor: Option<&str>,
        _id: &str,
        _patch: &CasterUpdate,
    ) -> Result<CasterDto, IdentityError> {
        Err(IdentityError::Upstream("not implemented".into()))
    }
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        if self.dto.username == username && password == "pw" {
            Ok(self.dto.clone())
        } else {
            Err(IdentityError::InvalidCredentials)
        }
    }

    async fn create_session(
        &self,
        caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        if self.dto.id != caster_id {
            return Err(IdentityError::NotFound);
        }
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        let csrf_token = format!("csrf-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: format!("sess-{n}"),
                caster: self.dto.clone(),
                csrf_token: csrf_token.clone(),
                idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
                absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
            },
        );
        Ok(CreatedSessionDto {
            id: format!("sess-{n}"),
            token,
            csrf_token,
            idle_expires_at: "2999-01-01T00:00:00.000Z".into(),
            absolute_expires_at: "2999-01-01T00:00:00.000Z".into(),
        })
    }

    async fn validate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionValidationDto>, IdentityError> {
        Ok(self.sessions.lock().unwrap().get(token).cloned())
    }

    async fn revoke_session(&self, _session_id: &str) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn get_caster(&self, caster_id: &str) -> Result<CasterDto, IdentityError> {
        if self.dto.id == caster_id {
            Ok(self.dto.clone())
        } else {
            Err(IdentityError::NotFound)
        }
    }

    async fn change_password(
        &self,
        _c: &str,
        _p: &str,
        _k: Option<&str>,
    ) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn change_stream_password(
        &self,
        _c: &str,
        _p: Option<&str>,
    ) -> Result<Option<String>, IdentityError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn config() -> Config {
    Config::new(
        "127.0.0.1:8081".into(),
        "https://portal.example.org".into(),
        "http://scheduler:8080".into(),
        "svc-token".into(),
        7,
        30,
    )
    .unwrap()
}

fn router(
    perms: BTreeMap<String, bool>,
    backend: &zukka_portal::api_client::FakeScheduleBackend,
) -> Router {
    let state = AppState::with_schedule_clients(
        config(),
        Arc::new(FakeIdentity::new(perms)),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(backend.schedule()),
        Arc::new(backend.shows()),
        Arc::new(backend.broadcasts()),
    );
    build_router(state)
}

fn caster_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("caster")
}

fn no_perms() -> BTreeMap<String, bool> {
    BTreeMap::new()
}

fn show(id: &str, name: &str, caster: &str) -> Show {
    Show {
        id: id.into(),
        slug: id.into(),
        name: name.into(),
        description: String::new(),
        artwork_url: None,
        links: Vec::new(),
        recurrence: None,
        caster_id: Some(caster.into()),
        created_by: caster.into(),
        created_at: "2026-11-19T12:00:00.000Z".into(),
        updated_at: "2026-11-19T12:00:00.000Z".into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn broadcast(
    id: &str,
    show_id: Option<&str>,
    caster: &str,
    title: &str,
    starts: &str,
    ends: Option<&str>,
) -> Broadcast {
    Broadcast {
        id: id.into(),
        show_id: show_id.map(str::to_string),
        caster_id: caster.into(),
        kind: "live".into(),
        title: title.into(),
        description: String::new(),
        starts_at: starts.into(),
        ends_at: ends.map(str::to_string),
        status: "scheduled".into(),
        tape_id: None,
        queue_id: None,
        version: 1,
        created_by: caster.into(),
        created_at: "2026-11-19T12:00:00.000Z".into(),
        updated_at: "2026-11-19T12:00:00.000Z".into(),
        warnings: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<String>,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(body) => builder.body(Body::from(body)).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

fn set_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        let first = raw.split(';').next().unwrap_or("");
        if let Some(rest) = first.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn meta_csrf(body: &str) -> Option<String> {
    let marker = "name=\"csrf-token\" content=\"";
    let start = body.find(marker)? + marker.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn cookie(session: &str) -> String {
    format!("__Host-zukka_session={session}")
}

async fn login(app: &Router) -> (String, String) {
    let (_, headers, body) = send(app, Method::GET, "/login", &[], None).await;
    let cookie_v = set_cookie_value(&headers, "__Host-zukka_csrf").expect("csrf cookie");
    let token = meta_csrf(&body).expect("csrf meta");
    let payload = form(&[
        ("username", "mira"),
        ("password", "pw"),
        ("csrf_token", &token),
    ]);
    let (status, headers, _) = send(
        app,
        Method::POST,
        "/login",
        &[
            ("cookie", &format!("__Host-zukka_csrf={cookie_v}")),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "login");
    let session = set_cookie_value(&headers, "__Host-zukka_session").expect("session cookie");
    let (_, _, dashboard) = send(
        &app.clone(),
        Method::GET,
        "/",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    let csrf = meta_csrf(&dashboard).expect("dashboard csrf");
    (session, csrf)
}

async fn post(
    app: &Router,
    session: &str,
    csrf: &str,
    uri: &str,
    htmx: bool,
    body: String,
) -> (StatusCode, HeaderMap, String) {
    let mut headers: Vec<(&str, &str)> = vec![
        ("cookie", ""),
        ("x-csrf-token", csrf),
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let cookie_header = cookie(session);
    headers[0] = ("cookie", &cookie_header);
    if htmx {
        headers.push(("hx-request", "true"));
    }
    send(app, Method::POST, uri, &headers, Some(body)).await
}

async fn get(app: &Router, session: &str, uri: &str, htmx: bool) -> (StatusCode, String) {
    let cookie_header = cookie(session);
    let mut headers: Vec<(&str, &str)> = vec![("cookie", &cookie_header)];
    if htmx {
        headers.push(("hx-request", "true"));
    }
    let (status, _, body) = send(app, Method::GET, uri, &headers, None).await;
    (status, body)
}

// ---------------------------------------------------------------------------
// US-3 / US-4: calendar page + fragment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn calendar_week_renders_berlin_labels() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        Some("s-1"),
        "c-1",
        "Late Show",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T22:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Late Show"), "{body}");
    assert!(body.contains("20:00–22:00"), "berlin time label: {body}");
    assert!(body.contains("id=\"calendar-grid\""), "{body}");
    assert!(body.contains("KW 47"), "week title: {body}");
}

#[tokio::test]
async fn calendar_fragment_only_on_hx_request() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18&fragment=calendar-grid",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("<html"), "fragment only: {body}");
    assert!(body.contains("id=\"calendar-grid\""), "{body}");
}

#[tokio::test]
async fn calendar_month_view_renders() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        None,
        "c-1",
        "Month Slot",
        "2026-11-18T20:00:00+01:00",
        None,
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(
        &app,
        &session,
        "/schedule?view=month&anchor=2026-11-15",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("November"), "{body}");
    assert!(body.contains("Month Slot"), "{body}");
    assert!(
        body.contains("out-of-month"),
        "adjacent-month cells: {body}"
    );
}

#[tokio::test]
async fn open_ended_broadcast_renders_open_bar() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        None,
        "c-1",
        "Open Live",
        "2026-11-18T20:00:00+01:00",
        None,
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (_, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert!(body.contains("offen"), "open badge: {body}");
    assert!(body.contains("20:00 →"), "open bar: {body}");
}

#[tokio::test]
async fn empty_calendar_has_defined_state() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(&app, &session, "/schedule", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("id=\"calendar-grid\""), "{body}");
    assert!(
        body.contains("Keine Einträge im sichtbaren Bereich"),
        "empty state shown inside the grid: {body}"
    );
}

// ---------------------------------------------------------------------------
// US-6: soft overlaps warn but never block (D12)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overlapping_broadcasts_render_warning() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        Some("s-1"),
        "c-1",
        "First",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T22:00:00+01:00"),
    ));
    backend.seed_broadcast(broadcast(
        "bc-2",
        Some("s-2"),
        "c-2",
        "Second",
        "2026-11-18T21:00:00+01:00",
        Some("2026-11-18T23:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (_, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert!(
        body.contains("conflict-warning"),
        "warning rendered: {body}"
    );
    assert!(body.contains("Überlappt"), "overlap message: {body}");
}

#[tokio::test]
async fn soft_overlap_does_not_block_create() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        Some("s-1"),
        "c-1",
        "First",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T22:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule",
        false,
        form(&[
            ("title", "Overlap OK"),
            ("kind", "live"),
            ("starts_at", "2026-11-18T21:00"),
            ("ends_at", "2026-11-18T23:00"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "soft overlap is allowed");
    assert!(
        backend
            .all_broadcasts()
            .iter()
            .any(|b| b.title == "Overlap OK"),
        "broadcast persisted"
    );
}

#[tokio::test]
async fn hard_self_overlap_is_409_with_toast() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_show(show("s-1", "Weekly", "c-1"));
    backend.seed_broadcast(broadcast(
        "seed-1",
        Some("s-1"),
        "c-1",
        "First",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T22:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, headers, body) = post(
        &app,
        &session,
        &csrf,
        "/schedule",
        true,
        form(&[
            ("title", "Second"),
            ("kind", "live"),
            ("show_id", "s-1"),
            ("starts_at", "2026-11-18T21:00"),
            ("ends_at", "2026-11-18T23:00"),
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "form re-render, not error page: {body}"
    );
    assert!(headers.get("hx-trigger").is_some(), "toast on conflict");
    assert!(body.contains("broadcast-form"), "form fragment: {body}");
    assert!(body.contains("überlappt"), "self-overlap message: {body}");
    assert_eq!(
        backend.all_broadcasts().len(),
        1,
        "hard conflict blocks the insert"
    );
}

// ---------------------------------------------------------------------------
// Reviewer blocker — date preview on the edit form keeps the edit context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_date_preview_keeps_edit_context() {
    // Regression: the date-change preview on the edit form is a GET to
    // `/schedule/new` with `hx-include="#broadcast-form"`. The form must carry
    // the broadcast `id` so the preview keeps the edit context
    // (`action="/schedule/{id}"` + `version`) — otherwise a date change turns
    // the edit into a create form.
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        None,
        "c-1",
        "Edit Me",
        "2026-11-18T20:00:00+01:00",
        None,
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;

    // 1) The edit page exposes the id in the form so htmx includes it.
    let (status, page) = get(&app, &session, "/schedule/seed-1/edit", false).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.contains("action=\"/schedule/seed-1\""),
        "edit action: {page}"
    );
    assert!(
        page.contains("name=\"id\" value=\"seed-1\""),
        "id carried in the form for the preview: {page}"
    );
    assert!(
        page.contains("name=\"version\" value=\"1\""),
        "version carried: {page}"
    );

    // 2) The preview (as htmx issues it, with the form's id) keeps the edit
    //    context instead of degrading into a create form.
    let (status, preview) = get(
        &app,
        &session,
        "/schedule/new?id=seed-1&version=1&title=Edit+Me&kind=live&starts_at=2026-11-18T21:00&ends_at=2026-11-18T23:00",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview never blocks: {preview}");
    assert!(
        preview.contains("action=\"/schedule/seed-1\""),
        "preview keeps the edit action: {preview}"
    );
    assert!(
        !preview.contains("action=\"/schedule\""),
        "preview must not fall back to the create action: {preview}"
    );
    assert!(
        preview.contains("name=\"version\" value=\"1\""),
        "preview keeps the version hidden field: {preview}"
    );
}

// ---------------------------------------------------------------------------
// US-5: create/edit/delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_converts_berlin_wall_time_to_utc() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule",
        false,
        form(&[
            ("title", "Winter"),
            ("kind", "live"),
            ("starts_at", "2026-11-18T20:00"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let created = backend
        .all_broadcasts()
        .into_iter()
        .find(|b| b.title == "Winter")
        .expect("created");
    assert_eq!(created.starts_at, "2026-11-18T19:00:00Z", "UTC wire value");
    assert_eq!(created.caster_id, "c-1", "caster from session");
}

#[tokio::test]
async fn create_playlist_without_queue_is_inline_error() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/schedule",
        true,
        form(&[
            ("title", "Bad"),
            ("kind", "playlist"),
            ("starts_at", "2026-11-18T20:00"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("queue_id"), "inline field error: {body}");
}

#[tokio::test]
async fn create_end_before_start_is_inline_error() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/schedule",
        true,
        form(&[
            ("title", "Backwards"),
            ("kind", "live"),
            ("starts_at", "2026-11-18T22:00"),
            ("ends_at", "2026-11-18T20:00"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("after starts_at"), "inline error: {body}");
    assert!(backend.all_broadcasts().is_empty());
}

#[tokio::test]
async fn update_end_before_start_keeps_edit_action() {
    // Regression: a `422` on `POST /schedule/{id}` (edit) must re-render the
    // form fragment still targeting `/schedule/{id}` (with its `version` hidden
    // field) — losing the id would turn a corrected save into a *new* broadcast.
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        None,
        "c-1",
        "Edit Me",
        "2026-11-18T20:00:00+01:00",
        None,
    ));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/schedule/seed-1",
        true,
        form(&[
            ("title", "Backwards"),
            ("kind", "live"),
            ("starts_at", "2026-11-18T22:00"),
            ("ends_at", "2026-11-18T20:00"),
            ("version", "1"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("after starts_at"), "inline error: {body}");
    assert!(
        body.contains("action=\"/schedule/seed-1\""),
        "edit action preserved: {body}"
    );
    assert!(
        !body.contains("action=\"/schedule\""),
        "must not fall back to create action: {body}"
    );
    assert!(
        body.contains("name=\"version\" value=\"1\""),
        "version hidden field preserved: {body}"
    );
    // The failed update must not have created a new broadcast.
    assert_eq!(backend.all_broadcasts().len(), 1);
    assert_eq!(backend.all_broadcasts()[0].title, "Edit Me");
}

#[tokio::test]
async fn create_requires_broadcast_write() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(no_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule",
        false,
        form(&[
            ("title", "Nope"),
            ("kind", "live"),
            ("starts_at", "2026-11-18T20:00"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn edit_form_prefills_berlin_time() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        None,
        "c-1",
        "Edit Me",
        "2026-11-18T20:00:00+01:00",
        None,
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(&app, &session, "/schedule/seed-1/edit", false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Edit Me"), "{body}");
    assert!(body.contains("2026-11-18T20:00"), "berlin prefill: {body}");
}

#[tokio::test]
async fn update_and_delete_broadcast() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        None,
        "c-1",
        "Old",
        "2026-11-18T20:00:00+01:00",
        None,
    ));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/seed-1",
        false,
        form(&[
            ("title", "New"),
            ("kind", "live"),
            ("starts_at", "2026-11-18T20:00"),
            ("version", "1"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(backend.all_broadcasts()[0].title, "New");

    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/seed-1/delete",
        false,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(backend.all_broadcasts().is_empty());
}

// ---------------------------------------------------------------------------
// US-2: shows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shows_list_and_create() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_show(show("s-1", "Morning Show", "c-1"));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, body) = get(&app, &session, "/shows", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Morning Show"), "{body}");

    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/shows",
        false,
        form(&[("slug", "evening"), ("name", "Evening Show")]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(backend.all_shows().iter().any(|s| s.name == "Evening Show"));
}

#[tokio::test]
async fn show_detail_renders() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_show(show("s-1", "Morning Show", "c-1"));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(&app, &session, "/shows/s-1", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Morning Show"), "{body}");
}

#[tokio::test]
async fn show_create_htmx_error_is_visible() {
    // Regression (reviewer minor): an htmx `422` on the show form must render
    // the error into the form's error slot — `hx-swap="none"` used to discard
    // it, leaving the user without feedback.
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/shows",
        true,
        form(&[
            ("slug", "broken"),
            ("name", "Broken"),
            ("recurrence", "FREQ=WEEKLY"),
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "htmx re-render, not an error page: {body}"
    );
    assert!(body.contains("alert-error"), "visible error block: {body}");
    assert!(
        body.contains("invalid recurrence rule"),
        "error message shown: {body}"
    );
    assert!(!body.contains("<html"), "fragment only: {body}");
    assert!(backend.all_shows().is_empty(), "nothing persisted");
}

#[tokio::test]
async fn invalid_recurrence_is_422() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/shows",
        false,
        form(&[
            ("slug", "broken"),
            ("name", "Broken"),
            ("recurrence", "FREQ=WEEKLY"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(backend.all_shows().is_empty(), "nothing persisted");
}

#[tokio::test]
async fn show_create_requires_show_write() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(no_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/shows",
        false,
        form(&[("slug", "x"), ("name", "X")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn broadcast_write_gates_new_form() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(no_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, _) = get(&app, &session, "/schedule/new", false).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// D12 — inline warnings in the form *before* saving (date-change preview)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn form_preview_shows_conflict_warnings_inline() {
    // Two overlapping broadcasts already in the window: the date-change
    // preview (`GET /schedule/new` as htmx fragment) must surface the
    // scheduler warnings inline and stay permissive (HTTP 200, no block).
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "seed-1",
        Some("s-1"),
        "c-1",
        "First",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T22:00:00+01:00"),
    ));
    backend.seed_broadcast(broadcast(
        "seed-2",
        Some("s-2"),
        "c-2",
        "Second",
        "2026-11-18T21:00:00+01:00",
        Some("2026-11-18T23:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(
        &app,
        &session,
        "/schedule/new?starts_at=2026-11-18T21:00&ends_at=2026-11-18T23:00",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview never blocks: {body}");
    assert!(body.contains("broadcast-form"), "form fragment: {body}");
    assert!(body.contains("conflict-warning"), "inline warning: {body}");
    assert!(body.contains("Überlappt"), "overlap message: {body}");
}

// ---------------------------------------------------------------------------
// P5 DoD — weekly show across the fall-back DST transition renders correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn calendar_renders_same_berlin_time_across_dst() {
    // Acceptance/P5 DoD: a weekly slot keeps the same local wall time across
    // the DST fall-back (2026-10-25). The portal calendar renders Berlin
    // labels for both sides of the transition; their UTC offsets differ.
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(broadcast(
        "summer",
        Some("s-1"),
        "c-1",
        "Summer Slot",
        "2026-10-14T20:00:00+02:00",
        Some("2026-10-14T22:00:00+02:00"),
    ));
    backend.seed_broadcast(broadcast(
        "winter",
        Some("s-1"),
        "c-1",
        "Winter Slot",
        "2026-10-28T20:00:00+01:00",
        Some("2026-10-28T22:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(
        &app,
        &session,
        "/schedule?view=month&anchor=2026-10-15",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Summer Slot"), "{body}");
    assert!(body.contains("Winter Slot"), "{body}");
    assert_eq!(
        body.matches("20:00–22:00").count(),
        2,
        "both sides of the DST transition render 20:00 Berlin: {body}"
    );
}

// ---------------------------------------------------------------------------
// Issue #119: broadcast lifecycle (start/end/cancel)
// ---------------------------------------------------------------------------

fn seeded(status: &str, starts: &str, ends: Option<&str>) -> Broadcast {
    let mut b = broadcast("bc-1", None, "c-1", "Live Show", starts, ends);
    b.status = status.into();
    b
}

/// B1 — htmx `start` → `204` + refresh + success toast, store `live`.
#[tokio::test]
async fn start_htmx_refreshes_with_success_toast() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/start",
        true,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers.get("hx-refresh").unwrap(), "true");
    let trigger = headers.get("hx-trigger").expect("toast").to_str().unwrap();
    assert!(trigger.contains("\"level\":\"success\""), "{trigger}");
    assert_eq!(backend.all_broadcasts()[0].status, "live");
}

/// B2 — `start` without a CSRF token → `403`, store unchanged.
#[tokio::test]
async fn start_without_csrf_is_403() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let cookie_header = cookie(&session);
    let (status, _, _) = send(
        &app,
        Method::POST,
        "/schedule/bc-1/start",
        &[
            ("cookie", &cookie_header),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        Some(String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(backend.all_broadcasts()[0].status, "scheduled");
}

/// B3 — caster without `broadcast.write` → `403`, store unchanged.
#[tokio::test]
async fn start_without_permission_is_403() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(no_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/start",
        false,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(backend.all_broadcasts()[0].status, "scheduled");
}

/// B4 — non-htmx `end` on `live` → `303 /schedule`, store `ended`.
#[tokio::test]
async fn end_non_htmx_redirects_and_mutates() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("live", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/end",
        false,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get(header::LOCATION).unwrap(), "/schedule");
    assert_eq!(backend.all_broadcasts()[0].status, "ended");
}

/// B5 — htmx `cancel` on `scheduled` → success toast, entry leaves the grid.
#[tokio::test]
async fn cancel_htmx_and_gone_from_grid() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _headers, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/cancel",
        true,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(backend.all_broadcasts()[0].status, "cancelled");
    let (_, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert!(!body.contains("bc-1"), "cancelled entry hidden: {body}");
}

/// B6 — illegal transition: htmx → error toast; non-htmx → `409`.
#[tokio::test]
async fn illegal_transition_toast_and_409() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("live", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;

    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/start",
        true,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let trigger = headers.get("hx-trigger").expect("toast").to_str().unwrap();
    assert!(trigger.contains("\"level\":\"error\""), "{trigger}");

    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/start",
        false,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// B7 — `cancel` on an unknown id → `404`.
#[tokio::test]
async fn cancel_unknown_is_404() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/nope/cancel",
        false,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// B8 — a `scheduled` card (with `broadcast.write`) shows Start + Abbrechen.
#[tokio::test]
async fn scheduled_card_shows_start_and_cancel() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (_, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert!(
        body.contains("/schedule/bc-1/start"),
        "start button: {body}"
    );
    assert!(
        body.contains("/schedule/bc-1/cancel"),
        "cancel button: {body}"
    );
}

/// B9 — a `live` card shows Ende + Abbrechen but no Start.
#[tokio::test]
async fn live_card_shows_end_and_cancel_only() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("live", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (_, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert!(body.contains("/schedule/bc-1/end"), "end button: {body}");
    assert!(
        body.contains("/schedule/bc-1/cancel"),
        "cancel button: {body}"
    );
    assert!(
        !body.contains("/schedule/bc-1/start"),
        "no start button: {body}"
    );
}

/// B10 — without `broadcast.write` the card has no action buttons.
#[tokio::test]
async fn card_without_permission_has_no_buttons() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(no_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (_, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert!(!body.contains("/schedule/bc-1/start"), "no start: {body}");
    assert!(!body.contains("/schedule/bc-1/cancel"), "no cancel: {body}");
}

/// B11 — htmx `cancel` on a `live` broadcast → success toast, store `cancelled`.
/// (Fake-only A6 previously covered the transition; this drives the real route.)
#[tokio::test]
async fn cancel_live_htmx_becomes_cancelled() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("live", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/cancel",
        true,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let trigger = headers.get("hx-trigger").expect("toast").to_str().unwrap();
    assert!(trigger.contains("\"level\":\"success\""), "{trigger}");
    assert_eq!(backend.all_broadcasts()[0].status, "cancelled");
}

/// B12 — non-htmx `end` on a `scheduled` broadcast → `409`, store unchanged.
/// (Illegal transition through the `/end` route; the fake-only A4 covered the
/// transition itself.)
#[tokio::test]
async fn end_scheduled_non_htmx_is_409() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded("scheduled", "2026-11-18T20:00:00+01:00", None));
    let app = router(caster_perms(), &backend);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/schedule/bc-1/end",
        false,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(backend.all_broadcasts()[0].status, "scheduled");
}

/// B13 — an `ended` card (still visible in the grid) exposes no action buttons.
#[tokio::test]
async fn ended_card_has_no_action_buttons() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded(
        "ended",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T21:00:00+01:00"),
    ));
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;
    let (status, body) = get(
        &app,
        &session,
        "/schedule?view=week&anchor=2026-11-18",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("bc-1"), "ended entry still visible: {body}");
    assert!(!body.contains("/schedule/bc-1/start"), "no start: {body}");
    assert!(!body.contains("/schedule/bc-1/end"), "no end: {body}");
    assert!(!body.contains("/schedule/bc-1/cancel"), "no cancel: {body}");
}

/// B14 — the edit form of an `ended`/`cancelled` broadcast shows no action
/// buttons (the `cancelled` card is hidden from the grid entirely, so the form
/// is where the status-gating must still hold).
#[tokio::test]
async fn ended_and_cancelled_forms_have_no_action_buttons() {
    let backend = zukka_portal::api_client::FakeScheduleBackend::new();
    backend.seed_broadcast(seeded(
        "ended",
        "2026-11-18T20:00:00+01:00",
        Some("2026-11-18T21:00:00+01:00"),
    ));
    backend.seed_broadcast({
        let mut c = seeded("cancelled", "2026-11-19T20:00:00+01:00", None);
        c.id = "bc-2".into();
        c
    });
    let app = router(caster_perms(), &backend);
    let (session, _csrf) = login(&app).await;

    for id in ["bc-1", "bc-2"] {
        let (status, body) = get(&app, &session, &format!("/schedule/{id}/edit"), false).await;
        assert_eq!(status, StatusCode::OK, "{id}: {body}");
        assert!(
            !body.contains(&format!("/schedule/{id}/start")),
            "{id}: no start: {body}"
        );
        assert!(
            !body.contains(&format!("/schedule/{id}/end")),
            "{id}: no end: {body}"
        );
        assert!(
            !body.contains(&format!("/schedule/{id}/cancel")),
            "{id}: no cancel: {body}"
        );
    }
}
