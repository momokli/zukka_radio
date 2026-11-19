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
