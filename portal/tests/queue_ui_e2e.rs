//! End-to-end tests for the Queue-UI (Issue #20: US-1…US-9) through the real
//! portal router.
//!
//! The scheduler is replaced by an in-memory [`FakeQueueApi`] (which enforces
//! the same invariants as the scheduler) and a [`FakeControlApi`]; the
//! music/job/waveform/track APIs are inert fakes. Login uses the real session
//! cookie + CSRF flow. Explicit proofs:
//!   (a) a reorder survives a reload (version guard),
//!   (b) a foreign queue is a `404` for an `.own`-only viewer,
//!   (c) a `409 version_conflict` reloads the authoritative fragment + toast.

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
    CasterDto, CreatedSessionDto, FakeControlApi, FakeQueueApi, FakeTrackApi, IdentityApi,
    IdentityError, JobError, NewUploadJob, NewWaveform, Queue, QueueApi, QueueError, QueueItem,
    SessionValidationDto, UploadJob, UploadJobApi, UploadJobPatch, Waveform, WaveformApi,
    WaveformApiError,
};
use zukka_portal::app::{AppState, build_router};
use zukka_portal::config::Config;
use zukka_portal::music_api::FakeMusicApi;
use zukka_portal::rate_limit::WindowLimiter;
use zukka_portal::rbac;

// ---------------------------------------------------------------------------
// Fake identity (one caster whose permission map is configurable)
// ---------------------------------------------------------------------------

struct FakeCaster {
    dto: CasterDto,
}

struct FakeIdentity {
    casters: Mutex<Vec<FakeCaster>>,
    sessions: Mutex<HashMap<String, SessionValidationDto>>,
    counter: Mutex<u64>,
}

impl FakeIdentity {
    fn new(perms: BTreeMap<String, bool>) -> Self {
        let dto = CasterDto {
            id: "c-1".into(),
            username: "mira".into(),
            display_name: "Mira".into(),
            email: None,
            role: "caster".into(),
            is_active: true,
            harbor_mount: None,
            stream_username: None,
            permissions: perms,
        };
        Self {
            casters: Mutex::new(vec![FakeCaster { dto }]),
            sessions: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
        }
    }
}

#[async_trait]
impl IdentityApi for FakeIdentity {
    async fn verify(&self, username: &str, password: &str) -> Result<CasterDto, IdentityError> {
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.username == username && password == "pw")
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::InvalidCredentials)
    }

    async fn create_session(
        &self,
        caster_id: &str,
        _ip: Option<&str>,
        _ua: Option<&str>,
    ) -> Result<CreatedSessionDto, IdentityError> {
        let caster = self
            .casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.id == caster_id)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::NotFound)?;
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let n = *counter;
        let token = format!("tok-{n}");
        let csrf_token = format!("csrf-{n}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionValidationDto {
                session_id: format!("sess-{n}"),
                caster,
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
        self.casters
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.dto.id == caster_id)
            .map(|c| c.dto.clone())
            .ok_or(IdentityError::NotFound)
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
// Inert fakes
// ---------------------------------------------------------------------------

#[derive(Default)]
struct NoJobs;

#[async_trait]
impl UploadJobApi for NoJobs {
    async fn create_job(&self, _job: &NewUploadJob) -> Result<String, JobError> {
        Err(JobError::not_found())
    }
    async fn patch_job(&self, _id: &str, _p: &UploadJobPatch) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn get_job(&self, _id: &str) -> Result<UploadJob, JobError> {
        Err(JobError::not_found())
    }
    async fn list_jobs(&self, _limit: i64) -> Result<Vec<UploadJob>, JobError> {
        Ok(vec![])
    }
}

#[derive(Default)]
struct NoWaveforms;

#[async_trait]
impl WaveformApi for NoWaveforms {
    async fn put_waveform(&self, _w: &NewWaveform) -> Result<Waveform, WaveformApiError> {
        Err(WaveformApiError::Upstream("noop".into()))
    }
    async fn get_waveform(
        &self,
        _k: &str,
        _id: &str,
    ) -> Result<Option<Waveform>, WaveformApiError> {
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
    queues: Arc<FakeQueueApi>,
    control: Arc<FakeControlApi>,
) -> Router {
    let state = AppState::with_clients(
        config(),
        Arc::new(FakeIdentity::new(perms)),
        Arc::new(WindowLimiter::new(50, Duration::from_secs(900))),
        Arc::new(FakeMusicApi::new()),
        Arc::new(NoJobs),
        Arc::new(NoWaveforms),
        Arc::new(FakeTrackApi::new()),
        queues,
        control,
    );
    build_router(state)
}

fn caster_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("caster")
}

fn operator_perms() -> BTreeMap<String, bool> {
    rbac::permissions_for_role("operator")
}

fn no_perms() -> BTreeMap<String, bool> {
    BTreeMap::new()
}

fn queue(id: &str, name: &str, caster: &str, version: i64) -> Queue {
    Queue {
        id: id.into(),
        caster_id: caster.into(),
        name: name.into(),
        description: String::new(),
        kind: "background".into(),
        version,
        created_at: "2026-11-19T12:00:00.000Z".into(),
        updated_at: "2026-11-19T12:00:00.000Z".into(),
    }
}

fn item(id: &str, queue_id: &str, position: i64) -> QueueItem {
    QueueItem {
        id: id.into(),
        queue_id: queue_id.into(),
        position,
        kind: "track".into(),
        track_id: Some(format!("t-{id}")),
        tape_id: None,
        link_url: None,
        link_provider: None,
        link_state: None,
        title_override: None,
        state: "pending".into(),
        played_at: None,
        created_at: "2026-11-19T12:00:00.000Z".into(),
    }
}

fn seeded() -> (Arc<FakeQueueApi>, Arc<FakeControlApi>) {
    let queues = Arc::new(FakeQueueApi::new());
    queues.seed_queue(queue("q-own", "My Queue", "c-1", 1));
    queues.seed_queue(queue("q-foreign", "Foreign Queue", "c-2", 1));
    let control = Arc::new(FakeControlApi::new());
    (queues, control)
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

/// POST helper: authenticated + CSRF header + same-origin signal.
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

// ---------------------------------------------------------------------------
// US-1 / US-3: overview + read-only foreign queue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_lists_own_and_foreign() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control);
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/queues",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("My Queue"), "{body}");
    assert!(body.contains("Foreign Queue"), "{body}");
    assert!(body.contains("read-only"), "foreign marked read-only");
    assert!(body.contains("href=\"/queues/q-own\""), "{body}");
}

#[tokio::test]
async fn foreign_editor_is_read_only() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control);
    let (session, _csrf) = login(&app).await;
    let (status, _, body) = send(
        &app,
        Method::GET,
        "/queues/q-foreign",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Foreign Queue"));
    assert!(
        !body.contains("aria-label=\"nach oben\""),
        "no move buttons"
    );
    assert!(
        !body.contains("action=\"/queues/q-foreign/items\""),
        "no add form"
    );
    assert!(!body.contains("/rename"), "no rename form");
    assert!(!body.contains("/skip"), "no skip form");
}

// ---------------------------------------------------------------------------
// US-2: create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_forces_own_caster_id() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    // A caster-only viewer cannot target another caster: caster_id is ignored.
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/queues",
        false,
        form(&[("name", "Fresh"), ("caster_id", "c-2")]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "redirect");
    let created = queues
        .all_queues()
        .into_iter()
        .find(|q| q.name == "Fresh")
        .expect("created");
    assert_eq!(created.caster_id, "c-1", "caster_id forced to viewer");
}

#[tokio::test]
async fn create_empty_name_is_422() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/queues",
        false,
        form(&[("name", "   ")]),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("name"), "{body}");
}

#[tokio::test]
async fn create_empty_name_htmx_renders_form_fragment() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/queues",
        true,
        form(&[("name", "   ")]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("name must not be empty"), "{body}");
    assert!(!body.contains("<html"), "fragment only");
}

// ---------------------------------------------------------------------------
// US-4: add items (track + link)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_link_item_sets_pending_link_state() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/items",
        true,
        form(&[
            ("kind", "link"),
            ("link_provider", "spotify"),
            ("link_url", "https://open.spotify.com/track/x"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = queues.all_items();
    let link = items.iter().find(|i| i.kind == "link").expect("link item");
    assert_eq!(link.link_provider.as_deref(), Some("spotify"));
    assert_eq!(link.link_state.as_deref(), Some("pending"));
    assert_eq!(link.position, 0);
}

/// Proof (d), second half: the created `kind=link` item (provider + URL) is
/// **rendered** in the authoritative htmx fragment — provider badge, the URL as
/// the title, and the pending-state badge.
#[tokio::test]
async fn add_link_item_renders_provider_and_url() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    let url = "https://open.spotify.com/track/xyz";
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/items",
        true,
        form(&[
            ("kind", "link"),
            ("link_provider", "spotify"),
            ("link_url", url),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("<html"), "fragment only");
    assert!(
        body.contains(r#"class="link-provider badge">spotify"#),
        "provider badge rendered: {body}"
    );
    assert!(body.contains(url), "url rendered as title: {body}");
    assert!(
        body.contains("wird aufgelöst"),
        "pending link-state badge rendered: {body}"
    );
    let link = queues
        .all_items()
        .into_iter()
        .find(|i| i.kind == "link")
        .expect("link item persisted");
    assert_eq!(link.link_url.as_deref(), Some(url));
    assert_eq!(link.link_provider.as_deref(), Some("spotify"));
}

// ---------------------------------------------------------------------------
// US-5: reorder (proof a + c)
// ---------------------------------------------------------------------------

fn three_items() -> Arc<FakeQueueApi> {
    let queues = Arc::new(FakeQueueApi::new());
    queues.seed_queue(queue("q-own", "My Queue", "c-1", 1));
    queues.seed_item(item("a", "q-own", 0));
    queues.seed_item(item("b", "q-own", 1));
    queues.seed_item(item("c", "q-own", 2));
    queues
}

#[tokio::test]
async fn move_reorders_and_survives_reload() {
    let queues = three_items();
    let app = router(
        caster_perms(),
        queues.clone(),
        Arc::new(FakeControlApi::new()),
    );
    let (session, csrf) = login(&app).await;

    // Move "a" after "b": expect b, a, c.
    let (status, headers, body) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/items/a/move",
        true,
        form(&[("after_id", "b"), ("version", "1")]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.contains_key("hx-trigger"), "success toast");

    // Proof (a): reload the editor and confirm the persisted order + version.
    let (_, _, page) = send(
        &app,
        Method::GET,
        "/queues/q-own",
        &[("cookie", &cookie(&session))],
        None,
    )
    .await;
    let pos_b = page.find("id=\"queue-item-b\"").expect("b present");
    let pos_a = page.find("id=\"queue-item-a\"").expect("a present");
    assert!(pos_b < pos_a, "b now precedes a: {page}");
    let queue = queues
        .all_queues()
        .into_iter()
        .find(|q| q.id == "q-own")
        .unwrap();
    assert_eq!(queue.version, 2, "version incremented once");
}

#[tokio::test]
async fn move_with_stale_version_reloads_with_toast() {
    let queues = three_items();
    let app = router(
        caster_perms(),
        queues.clone(),
        Arc::new(FakeControlApi::new()),
    );
    let (session, csrf) = login(&app).await;

    // Proof (c): a stale version → authoritative fragment + error toast.
    let (status, headers, body) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/items/a/move",
        true,
        form(&[("after_id", "b"), ("version", "999")]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fragment, not an error page: {body}"
    );
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .expect("hx-trigger toast");
    assert!(trigger.contains("toast"), "{trigger}");
    assert!(
        trigger.contains("version") || trigger.contains("neu geladen"),
        "{trigger}"
    );
    // The authoritative order is unchanged (a, b, c).
    let pos_a = body.find("id=\"queue-item-a\"").expect("a");
    let pos_b = body.find("id=\"queue-item-b\"").expect("b");
    assert!(pos_a < pos_b, "server truth preserved: {body}");
    assert_eq!(
        queues
            .all_queues()
            .into_iter()
            .find(|q| q.id == "q-own")
            .unwrap()
            .version,
        1,
        "no write on conflict"
    );
}

// ---------------------------------------------------------------------------
// RBAC / ownership (proof b)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn own_only_foreign_queue_is_404() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-foreign/items",
        false,
        form(&[("kind", "track"), ("track_id", "t-1")]),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign invisible to .own");
}

#[tokio::test]
async fn any_permission_may_write_foreign_queue() {
    let (queues, control) = seeded();
    let app = router(operator_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-foreign/items",
        false,
        form(&[("kind", "track"), ("track_id", "t-1")]),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "operator may write");
    assert!(queues.all_items().iter().any(|i| i.queue_id == "q-foreign"));
}

#[tokio::test]
async fn missing_permission_is_forbidden() {
    let (queues, control) = seeded();
    let app = router(no_perms(), queues, control);
    let (session, csrf) = login(&app).await;
    let (status, _, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/items",
        false,
        form(&[("kind", "track"), ("track_id", "t-1")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// US-7 / US-9: rename + delete (version)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rename_with_correct_version_refreshes() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/rename",
        true,
        form(&[("name", "Renamed"), ("version", "1")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers.get("hx-refresh").and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let renamed = queues
        .all_queues()
        .into_iter()
        .find(|q| q.id == "q-own")
        .unwrap();
    assert_eq!(renamed.name, "Renamed");
    assert_eq!(renamed.version, 2);
}

#[tokio::test]
async fn rename_with_stale_version_toasts() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    let (status, headers, body) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/rename",
        true,
        form(&[("name", "Renamed"), ("version", "42")]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("hx-trigger").is_some(), "toast on conflict");
    assert!(body.contains("queue-items"), "authoritative fragment");
}

#[tokio::test]
async fn delete_queue_with_correct_version_redirects() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues.clone(), control);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/delete",
        true,
        form(&[("version", "1")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers.get("hx-redirect").and_then(|v| v.to_str().ok()),
        Some("/queues")
    );
    assert!(!queues.all_queues().iter().any(|q| q.id == "q-own"));
}

// ---------------------------------------------------------------------------
// US-6: delete item
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_item_updates_list() {
    let queues = three_items();
    let app = router(
        caster_perms(),
        queues.clone(),
        Arc::new(FakeControlApi::new()),
    );
    let (session, csrf) = login(&app).await;
    let (status, _, body) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/items/b/delete",
        true,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("id=\"queue-item-b\""), "{body}");
    assert!(queues.all_items().iter().all(|i| i.id != "b"));
}

// ---------------------------------------------------------------------------
// US-8: skip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn skip_calls_control_api() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control.clone());
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/skip",
        true,
        form(&[("reason", "manual")]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(control.calls(), vec!["manual".to_string()]);
    assert!(headers.get("hx-trigger").is_some());
}

#[tokio::test]
async fn skip_conflict_is_a_friendly_toast() {
    let (queues, control) = seeded();
    control.fail_with("nothing playing");
    let app = router(caster_perms(), queues, control);
    let (session, csrf) = login(&app).await;
    let (status, headers, _) = post(
        &app,
        &session,
        &csrf,
        "/queues/q-own/skip",
        true,
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "no error page");
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert!(trigger.contains("aktive Quelle"), "{trigger}");
}

// ---------------------------------------------------------------------------
// Static asset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sortable_js_is_served() {
    let (queues, control) = seeded();
    let app = router(caster_perms(), queues, control);
    let (status, headers, body) =
        send(&app, Method::GET, "/static/sortable.min.js", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    assert!(body.contains("Sortable"), "vendored SortableJS");
}

/// Sanity: the fake enforces the scheduler's exactly-one-content invariant.
#[tokio::test]
async fn fake_queue_api_enforces_content_matrix() {
    let queues = FakeQueueApi::new();
    queues.seed_queue(queue("q-1", "Q", "c-1", 1));
    let bad = queues
        .add_item(
            None,
            "q-1",
            &zukka_portal::api_client::NewQueueItem {
                kind: "track".into(),
                track_id: None,
                ..Default::default()
            },
        )
        .await
        .expect_err("missing content");
    assert!(matches!(bad, QueueError::Validation { .. }), "{bad}");
}
