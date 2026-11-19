//! Queue handlers (Issue #20, US-1…US-9).
//!
//! The portal is thin (§4.2): every read/write goes through the [`QueueApi`]
//! write-through with the session `caster_id` as the viewer context. RBAC is
//! checked first, then ownership is computed **server-side** from
//! `Queue.caster_id` vs. `ctx.caster_id` (never from client input, §4):
//!
//! * `queue.write.any` → every queue is writable;
//! * `queue.write.own` → only the owner's queues; a foreign queue is a `404`
//!   (never a `403`, consistent with the library visibility rule §1);
//! * neither permission → `403` (via [`rbac::check`]).
//!
//! Every mutation answers with the scheduler's authoritative `queue-list`
//! fragment (or a `409` reload + `HX-Trigger` toast); the UI is never leading.

use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::api_client::{
    ControlError, NewQueue, NewQueueItem, Queue, QueueError, QueueItem, QueueQuery,
};
use crate::app::AppState;
use crate::auth::AuthContext;
use crate::error::{PortalError, redirect};
use crate::rbac;
use crate::templates::{
    QueueEditorPage, QueueFormFragment, QueueItemRow, QueueListFragment, QueueRow, QueuesPage,
};

fn render<T: Template>(page: &T) -> Result<Response, PortalError> {
    Ok(Html(page.render()?).into_response())
}

pub(crate) fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some()
}

fn queue_error(err: QueueError) -> PortalError {
    match err {
        QueueError::NotFound => PortalError::NotFound,
        QueueError::Validation { field, message } => PortalError::Validation { field, message },
        // Conflicts are handled explicitly at each call site; reaching here is
        // an unexpected upstream failure.
        QueueError::Conflict(message) | QueueError::Upstream(message) => {
            PortalError::Upstream(message)
        }
    }
}

// TODO(auth): The ownership check below (`can_write`/`authorize_write`) is a
// portal-side stopgap; the authoritative queue-scoped ownership must move into
// the scheduler's RBAC. Mirror of `scheduler/src/api/queues.rs` TODO(auth);
// tracked under Issue #16 (Auth) and specified in
// `docs/consolidated-architecture.md` §6.10.1.

/// `true` when the viewer may write `queue` (server-side ownership, §4).
fn can_write(ctx: &AuthContext, queue: &Queue) -> bool {
    if ctx.has("queue.write.any") {
        true
    } else if ctx.has("queue.write.own") {
        queue.caster_id == ctx.caster_id
    } else {
        false
    }
}

/// Checks permission **and** ownership; `Ok(queue)` only when the viewer may
/// write it. Foreign queue with only `.own` → `NotFound`; no permission → `403`.
async fn authorize_write(
    state: &AppState,
    ctx: &AuthContext,
    queue_id: &str,
) -> Result<Queue, PortalError> {
    let queue = state
        .queues
        .get(Some(&ctx.caster_id), queue_id)
        .await
        .map_err(queue_error)?;
    if ctx.has("queue.write.any") {
        return Ok(queue);
    }
    if ctx.has("queue.write.own") {
        if queue.caster_id == ctx.caster_id {
            return Ok(queue);
        }
        // Foreign queue is invisible to an `.own`-only viewer (§4, like §1).
        return Err(PortalError::NotFound);
    }
    Err(PortalError::Forbidden(
        "missing permission queue.write.own".into(),
    ))
}

/// Escapes a JSON value so every byte is visible ASCII — required because the
/// `HX-Trigger` header may only carry visible-ASCII bytes (umlauts, em dashes).
pub(crate) fn ascii_json(value: &serde_json::Value) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf).iter() {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Attaches an `HX-Trigger` toast to `response`.
pub(crate) fn set_toast(response: &mut Response, level: &str, message: &str) {
    let payload = serde_json::json!({ "toast": { "level": level, "message": message } });
    if let Ok(value) = header::HeaderValue::from_str(&ascii_json(&payload)) {
        response
            .headers_mut()
            .insert(header::HeaderName::from_static("hx-trigger"), value);
    }
}

/// Loads queue + items and renders the authoritative `queue-list` fragment,
/// optionally with a toast.
async fn render_list_fragment(
    state: &AppState,
    ctx: &AuthContext,
    queue_id: &str,
    toast: Option<(&str, &str)>,
) -> Result<Response, PortalError> {
    let queue = state
        .queues
        .get(Some(&ctx.caster_id), queue_id)
        .await
        .map_err(queue_error)?;
    let items = state
        .queues
        .items(Some(&ctx.caster_id), queue_id)
        .await
        .map_err(queue_error)?;
    let writable = can_write(ctx, &queue);
    let fragment = QueueListFragment {
        csrf_token: ctx.csrf_token.clone(),
        queue_id: queue.id.clone(),
        version: queue.version,
        items: QueueItemRow::from_items(items),
        can_write: writable,
    };
    let mut response = render(&fragment)?;
    if let Some((level, message)) = toast {
        set_toast(&mut response, level, message);
    }
    Ok(response)
}

/// `204` with an `HX-Refresh` so htmx reloads the page (server stays truth).
pub(crate) fn refresh_with_toast(level: &str, message: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::HeaderName::from_static("hx-refresh"),
        header::HeaderValue::from_static("true"),
    );
    set_toast(&mut response, level, message);
    response
}

/// `204` + toast only (no DOM target), for `hx-swap="none"` actions like skip.
pub(crate) fn toast_only(level: &str, message: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_toast(&mut response, level, message);
    response
}

// ---------------------------------------------------------------------------
// US-1 / US-2: overview + create
// ---------------------------------------------------------------------------

/// `GET /queues` — own + foreign queues (read-only for foreign), + create form.
pub async fn queues_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Response, PortalError> {
    let queues = state
        .queues
        .list(Some(&ctx.caster_id), &QueueQuery::default())
        .await
        .map_err(queue_error)?;

    let mut rows: Vec<QueueRow> = Vec::new();
    for queue in queues {
        let item_count = state
            .queues
            .items(Some(&ctx.caster_id), &queue.id)
            .await
            .map(|items| items.len() as i64)
            .unwrap_or(0);
        rows.push(QueueRow {
            can_write: can_write(&ctx, &queue),
            item_count,
            queue,
        });
    }
    // Own queues first, then foreign, each by name.
    rows.sort_by(|a, b| {
        let own_a = a.queue.caster_id == ctx.caster_id;
        let own_b = b.queue.caster_id == ctx.caster_id;
        own_b
            .cmp(&own_a)
            .then_with(|| a.queue.name.cmp(&b.queue.name))
    });

    render(&QueuesPage {
        csrf_token: ctx.csrf_token.clone(),
        can_create: ctx.has("queue.write.own"),
        queues: rows,
        error: None,
    })
}

/// Body of `POST /queues`.
#[derive(Debug, Deserialize)]
pub struct CreateQueueForm {
    pub name: String,
    pub description: Option<String>,
    pub kind: Option<String>,
    /// Honoured only with `queue.write.any`; ignored otherwise (§4).
    pub caster_id: Option<String>,
}

/// `POST /queues` — create a queue (own or, with `.any`, for another caster).
pub async fn queues_post(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Form(form): Form<CreateQueueForm>,
) -> Result<Response, PortalError> {
    rbac::check(&ctx, "queue.write.own")?;
    let caster_id = if ctx.has("queue.write.any") {
        form.caster_id
            .clone()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| ctx.caster_id.clone())
    } else {
        ctx.caster_id.clone()
    };
    let req = NewQueue {
        caster_id,
        name: form.name.clone(),
        description: form.description.clone().filter(|d| !d.trim().is_empty()),
        kind: form.kind.clone().filter(|k| !k.is_empty()),
    };

    match state.queues.create(Some(&ctx.caster_id), &req).await {
        Ok(_) => {
            if is_htmx(&headers) {
                Ok(refresh_with_toast("success", "Queue angelegt."))
            } else {
                Ok(redirect("/queues"))
            }
        }
        Err(QueueError::Validation { field, message }) => {
            if is_htmx(&headers) {
                render(&QueueFormFragment {
                    csrf_token: ctx.csrf_token.clone(),
                    error: Some(message),
                })
            } else {
                Err(PortalError::Validation { field, message })
            }
        }
        Err(err) => Err(queue_error(err)),
    }
}

// ---------------------------------------------------------------------------
// US-3: editor
// ---------------------------------------------------------------------------

/// `GET /queues/{id}` — items in position order + add/rename/skip/clear forms.
pub async fn queue_editor(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Response, PortalError> {
    let queue = state
        .queues
        .get(Some(&ctx.caster_id), &id)
        .await
        .map_err(queue_error)?;
    let items = state
        .queues
        .items(Some(&ctx.caster_id), &id)
        .await
        .map_err(queue_error)?;
    let writable = can_write(&ctx, &queue);
    render(&QueueEditorPage {
        csrf_token: ctx.csrf_token.clone(),
        queue_id: queue.id.clone(),
        version: queue.version,
        items: QueueItemRow::from_items(items),
        can_write: writable,
        can_control: ctx.has("control.write"),
        queue,
    })
}

// ---------------------------------------------------------------------------
// US-4: add item
// ---------------------------------------------------------------------------

/// Body of `POST /queues/{id}/items`.
#[derive(Debug, Deserialize)]
pub struct AddItemForm {
    pub kind: String,
    pub track_id: Option<String>,
    pub link_provider: Option<String>,
    pub link_url: Option<String>,
    pub title_override: Option<String>,
}

/// `POST /queues/{id}/items` — add a track or link item.
pub async fn item_add(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<AddItemForm>,
) -> Result<Response, PortalError> {
    authorize_write(&state, &ctx, &id).await?;
    let req = NewQueueItem {
        kind: form.kind.clone(),
        track_id: form.track_id.clone().filter(|s| !s.trim().is_empty()),
        tape_id: None,
        link_url: form.link_url.clone().filter(|s| !s.trim().is_empty()),
        link_provider: form.link_provider.clone().filter(|s| !s.trim().is_empty()),
        title_override: form.title_override.clone().filter(|s| !s.trim().is_empty()),
        position: None,
    };

    match state.queues.add_item(Some(&ctx.caster_id), &id, &req).await {
        Ok(_) => {
            if is_htmx(&headers) {
                render_list_fragment(&state, &ctx, &id, Some(("success", "Item hinzugefügt.")))
                    .await
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(QueueError::Validation { message, .. }) => {
            if is_htmx(&headers) {
                render_list_fragment(&state, &ctx, &id, Some(("error", &message))).await
            } else {
                Err(PortalError::Validation {
                    field: "kind".into(),
                    message,
                })
            }
        }
        Err(err) => Err(queue_error(err)),
    }
}

// ---------------------------------------------------------------------------
// US-5: move (reorder)
// ---------------------------------------------------------------------------

/// Body of `POST /queues/{id}/items/{item}/move`.
#[derive(Debug, Deserialize)]
pub struct MoveForm {
    pub before_id: Option<String>,
    pub after_id: Option<String>,
    pub version: Option<i64>,
}

/// Computes the full new id order: remove `item_id`, then re-insert before
/// `before_id` / after `after_id`. Unknown neighbours → `BadRequest`.
pub fn compute_order(
    items: &[QueueItem],
    item_id: &str,
    before_id: Option<&str>,
    after_id: Option<&str>,
) -> Result<Vec<String>, PortalError> {
    let mut ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let Some(pos) = ids.iter().position(|id| id == item_id) else {
        return Err(PortalError::NotFound);
    };
    ids.remove(pos);
    let index = if let Some(before) = before_id.filter(|b| !b.is_empty()) {
        ids.iter()
            .position(|id| id == before)
            .ok_or_else(|| PortalError::BadRequest(format!("unknown before_id {before}")))?
    } else if let Some(after) = after_id.filter(|a| !a.is_empty()) {
        ids.iter()
            .position(|id| id == after)
            .ok_or_else(|| PortalError::BadRequest(format!("unknown after_id {after}")))?
            + 1
    } else {
        return Err(PortalError::BadRequest(
            "before_id or after_id is required".into(),
        ));
    };
    ids.insert(index, item_id.to_string());
    Ok(ids)
}

/// `POST /queues/{id}/items/{item}/move` — reorder via the scheduler `reorder`.
///
/// The new order is computed server-side from the *current* items; the client's
/// order is never trusted (§5). A `409` reloads the authoritative fragment and
/// raises an error toast.
pub async fn item_move(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path((id, item)): Path<(String, String)>,
    Form(form): Form<MoveForm>,
) -> Result<Response, PortalError> {
    let queue = authorize_write(&state, &ctx, &id).await?;
    let items = state
        .queues
        .items(Some(&ctx.caster_id), &id)
        .await
        .map_err(queue_error)?;
    let order = compute_order(
        &items,
        &item,
        form.before_id.as_deref(),
        form.after_id.as_deref(),
    )?;
    let version = form.version.unwrap_or(queue.version);

    let outcome = state
        .queues
        .reorder(Some(&ctx.caster_id), &id, &order, version)
        .await;
    match outcome {
        Ok(_) => {
            if is_htmx(&headers) {
                render_list_fragment(
                    &state,
                    &ctx,
                    &id,
                    Some(("success", "Reihenfolge gespeichert.")),
                )
                .await
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(QueueError::Conflict(_)) => {
            if is_htmx(&headers) {
                render_list_fragment(
                    &state,
                    &ctx,
                    &id,
                    Some((
                        "error",
                        "Queue wurde zwischenzeitlich geändert — neu geladen.",
                    )),
                )
                .await
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(QueueError::Validation { message, .. }) => {
            if is_htmx(&headers) {
                render_list_fragment(&state, &ctx, &id, Some(("error", &message))).await
            } else {
                Err(PortalError::Validation {
                    field: "item_ids".into(),
                    message,
                })
            }
        }
        Err(err) => Err(queue_error(err)),
    }
}

// ---------------------------------------------------------------------------
// US-6: delete item
// ---------------------------------------------------------------------------

/// `POST /queues/{id}/items/{item}/delete` — delete one item.
pub async fn item_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path((id, item)): Path<(String, String)>,
) -> Result<Response, PortalError> {
    authorize_write(&state, &ctx, &id).await?;
    match state
        .queues
        .delete_item(Some(&ctx.caster_id), &id, &item)
        .await
    {
        Ok(()) => {
            if is_htmx(&headers) {
                render_list_fragment(&state, &ctx, &id, Some(("success", "Item entfernt."))).await
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(err) => Err(queue_error(err)),
    }
}

// ---------------------------------------------------------------------------
// US-7: rename
// ---------------------------------------------------------------------------

/// Body of `POST /queues/{id}/rename`.
#[derive(Debug, Deserialize)]
pub struct RenameForm {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<i64>,
}

/// `POST /queues/{id}/rename` — rename/update with optimistic lock.
pub async fn queue_rename(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<RenameForm>,
) -> Result<Response, PortalError> {
    let queue = authorize_write(&state, &ctx, &id).await?;
    let version = form.version.unwrap_or(queue.version);
    let outcome = state
        .queues
        .update(
            Some(&ctx.caster_id),
            &id,
            Some(&form.name),
            form.description.as_deref(),
            version,
        )
        .await;
    match outcome {
        Ok(_) => {
            if is_htmx(&headers) {
                Ok(refresh_with_toast("success", "Queue umbenannt."))
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(QueueError::Conflict(_)) => {
            if is_htmx(&headers) {
                render_list_fragment(
                    &state,
                    &ctx,
                    &id,
                    Some((
                        "error",
                        "Queue wurde zwischenzeitlich geändert — neu geladen.",
                    )),
                )
                .await
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(QueueError::Validation { field, message }) => {
            if is_htmx(&headers) {
                render_list_fragment(&state, &ctx, &id, Some(("error", &message))).await
            } else {
                Err(PortalError::Validation { field, message })
            }
        }
        Err(err) => Err(queue_error(err)),
    }
}

// ---------------------------------------------------------------------------
// US-8: skip (control/skip)
// ---------------------------------------------------------------------------

/// Body of `POST /queues/{id}/skip`.
#[derive(Debug, Deserialize)]
pub struct SkipForm {
    pub reason: Option<String>,
}

/// `POST /queues/{id}/skip` — global skip of the currently playing item (§6).
///
/// The scheduler has no queue-scoped skip; the portal calls `control/skip`. A
/// `409` is surfaced as a clear informational toast (never an error page).
pub async fn queue_skip(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<SkipForm>,
) -> Result<Response, PortalError> {
    // Skip is a global control action (`POST /control/skip`, §6.10.1): it is
    // bound to `control.write`, not to queue ownership. No queue.write gate.
    rbac::check(&ctx, "control.write")?;
    let reason = form
        .reason
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "portal skip".to_string());
    let response = match state.control.skip(&reason).await {
        Ok(()) => toast_only("success", "Skip gesendet."),
        Err(ControlError::Conflict(_)) => toast_only(
            "info",
            "Diese Queue ist gerade nicht die aktive Quelle — Skip wirkt auf das laufende Programm.",
        ),
        Err(ControlError::Upstream(_)) => toast_only("error", "Scheduler nicht erreichbar."),
    };
    if is_htmx(&headers) {
        Ok(response)
    } else {
        Ok(redirect(&format!("/queues/{id}")))
    }
}

// ---------------------------------------------------------------------------
// US-9: delete queue
// ---------------------------------------------------------------------------

/// Body of `POST /queues/{id}/delete`.
#[derive(Debug, Deserialize)]
pub struct DeleteQueueForm {
    pub version: Option<i64>,
}

/// `POST /queues/{id}/delete` — delete a queue with optimistic lock.
pub async fn queue_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<DeleteQueueForm>,
) -> Result<Response, PortalError> {
    let queue = authorize_write(&state, &ctx, &id).await?;
    let version = form.version.unwrap_or(queue.version);
    match state
        .queues
        .delete(Some(&ctx.caster_id), &id, version)
        .await
    {
        Ok(()) => {
            if is_htmx(&headers) {
                let mut response = StatusCode::NO_CONTENT.into_response();
                response.headers_mut().insert(
                    header::HeaderName::from_static("hx-redirect"),
                    header::HeaderValue::from_static("/queues"),
                );
                Ok(response)
            } else {
                Ok(redirect("/queues"))
            }
        }
        Err(QueueError::Conflict(_)) => {
            if is_htmx(&headers) {
                render_list_fragment(
                    &state,
                    &ctx,
                    &id,
                    Some((
                        "error",
                        "Queue wurde zwischenzeitlich geändert — neu geladen.",
                    )),
                )
                .await
            } else {
                Ok(redirect(&format!("/queues/{id}")))
            }
        }
        Err(err) => Err(queue_error(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, position: i64) -> QueueItem {
        QueueItem {
            id: id.into(),
            queue_id: "q-1".into(),
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
            created_at: String::new(),
        }
    }

    fn items() -> Vec<QueueItem> {
        vec![item("a", 0), item("b", 1), item("c", 2)]
    }

    #[test]
    fn move_before_first() {
        let order = compute_order(&items(), "c", Some("a"), None).unwrap();
        assert_eq!(order, vec!["c", "a", "b"]);
    }

    #[test]
    fn move_after_last() {
        let order = compute_order(&items(), "a", None, Some("c")).unwrap();
        assert_eq!(order, vec!["b", "c", "a"]);
    }

    #[test]
    fn move_middle_via_after() {
        let order = compute_order(&items(), "a", None, Some("b")).unwrap();
        assert_eq!(order, vec!["b", "a", "c"]);
    }

    #[test]
    fn move_unknown_neighbour_is_bad_request() {
        assert!(matches!(
            compute_order(&items(), "a", Some("zzz"), None),
            Err(PortalError::BadRequest(_))
        ));
    }

    #[test]
    fn move_unknown_item_is_not_found() {
        assert!(matches!(
            compute_order(&items(), "zzz", Some("a"), None),
            Err(PortalError::NotFound)
        ));
    }

    #[test]
    fn move_without_neighbour_is_bad_request() {
        assert!(matches!(
            compute_order(&items(), "a", None, None),
            Err(PortalError::BadRequest(_))
        ));
    }

    #[test]
    fn ascii_json_escapes_non_ascii() {
        let value = serde_json::json!({ "message": "geändert — ok" });
        let encoded = ascii_json(&value);
        assert!(encoded.is_ascii(), "{encoded}");
        assert!(encoded.contains("\\u"), "{encoded}");
        assert!(!encoded.contains('ä'), "{encoded}");
    }
}
