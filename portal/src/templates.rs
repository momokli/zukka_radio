//! Askama template structs (one per page).

use askama::Template;

use crate::api_client::{Queue, QueueItem, Show, Track, UploadJob};

/// Login page (`GET /login`).
#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    pub csrf_token: String,
    pub username: String,
    pub error: Option<String>,
}

/// Dashboard (`GET /`).
#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub csrf_token: String,
    pub display_name: String,
    pub role: String,
    pub caster_id: String,
}

/// Account page (`GET /account`).
#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountPage {
    pub csrf_token: String,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub harbor_mount: Option<String>,
    pub stream_username: Option<String>,
    /// Set exactly once, right after a generated stream password.
    pub stream_password: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// Standalone error page (no session, no layout).
#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub status: u16,
    pub message: String,
}

/// A track plus the per-viewer row capabilities. `can_toggle` is the
/// ownership comparison (`track.caster_id == viewer`), computed server-side in
/// [`crate::library`] — never from client input.
pub struct TrackRow {
    pub track: Track,
    pub can_toggle: bool,
}

/// Library page (`GET /library`): search + filter + track list + upload form.
#[derive(Template)]
#[template(path = "library.html")]
pub struct LibraryPage {
    pub csrf_token: String,
    pub jobs: Vec<UploadJob>,
    pub tracks: Vec<TrackRow>,
    pub q: String,
    pub visibility: String,
    pub owner: String,
    pub can_delete: bool,
    pub page: i64,
    pub prev_page: Option<i64>,
    pub next_page: Option<i64>,
}

/// Fragment returned for a search/filter request (`HX-Request` on `/library`):
/// the track list + paging only.
#[derive(Template)]
#[template(path = "partials/library_results.html")]
pub struct LibraryResultsFragment {
    pub csrf_token: String,
    pub tracks: Vec<TrackRow>,
    pub q: String,
    pub visibility: String,
    pub owner: String,
    pub can_delete: bool,
    pub page: i64,
    pub prev_page: Option<i64>,
    pub next_page: Option<i64>,
}

/// A single track row (visibility toggle result, `outerHTML` swap).
#[derive(Template)]
#[template(path = "partials/track_row.html")]
pub struct TrackRowFragment {
    pub csrf_token: String,
    pub row: TrackRow,
    pub can_delete: bool,
}

/// Track detail page (`GET /library/tracks/{id}`).
#[derive(Template)]
#[template(path = "track_detail.html")]
pub struct TrackDetailPage {
    pub csrf_token: String,
    pub track: Track,
    pub can_toggle: bool,
    pub can_delete: bool,
}

/// Fragment returned by `POST /library/upload`: the freshly created jobs.
#[derive(Template)]
#[template(path = "library_upload.html")]
pub struct LibraryUploadPage {
    pub csrf_token: String,
    pub jobs: Vec<UploadJob>,
}

/// Single job fragment (`GET /library/upload/{id}`), the polling target.
#[derive(Template)]
#[template(path = "partials/upload_job.html")]
pub struct UploadJobFragment {
    pub csrf_token: String,
    pub job: UploadJob,
}

/// Inline SVG rendering of a track waveform (D8, `partials/waveform.html`).
/// `peaks` is the interleaved 8-bit `(min, max)` pair sequence (`2 * bars`).
#[derive(Template)]
#[template(path = "partials/waveform.html")]
pub struct WaveformSvg {
    pub bars: i64,
    pub peaks: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Issue #20: queues (Queue-UI)
// ---------------------------------------------------------------------------

/// A queue plus the per-viewer row capabilities. `can_write` is the
/// server-side ownership comparison, computed in [`crate::queues`].
pub struct QueueRow {
    pub queue: Queue,
    pub can_write: bool,
    pub item_count: i64,
}

/// A queue item plus its neighbours (for the no-JS ↑/↓ buttons) and the
/// pre-computed display strings (title, link label/state). Built by
/// [`QueueItemRow::from_items`] so the templates stay free of method chains.
pub struct QueueItemRow {
    pub item: QueueItem,
    pub prev_id: String,
    pub next_id: String,
    pub title: String,
    pub link_label: String,
    pub link_state: String,
}

impl QueueItemRow {
    /// Builds the row list, wiring each item to its position neighbours.
    pub fn from_items(items: Vec<QueueItem>) -> Vec<Self> {
        let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
        items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let prev_id = index
                    .checked_sub(1)
                    .and_then(|i| ids.get(i).cloned())
                    .unwrap_or_default();
                let next_id = ids.get(index + 1).cloned().unwrap_or_default();
                let title = item
                    .title_override
                    .clone()
                    .or_else(|| item.track_id.clone())
                    .or_else(|| item.link_url.clone())
                    .unwrap_or_else(|| item.id.clone());
                let link_label = if item.kind == "link" {
                    item.link_provider
                        .clone()
                        .unwrap_or_else(|| "link".to_string())
                } else {
                    String::new()
                };
                let link_state = item.link_state.clone().unwrap_or_default();
                Self {
                    item,
                    prev_id,
                    next_id,
                    title,
                    link_label,
                    link_state,
                }
            })
            .collect()
    }
}

/// Queue overview (`GET /queues`): own + foreign queues + create form.
#[derive(Template)]
#[template(path = "queues.html")]
pub struct QueuesPage {
    pub csrf_token: String,
    pub can_create: bool,
    pub queues: Vec<QueueRow>,
    /// Inline create-form error (kept `None` on the plain overview).
    pub error: Option<String>,
}

/// Queue editor (`GET /queues/{id}`). Carries the fragment fields too so
/// `partials/queue_list.html` can be `{% include %}`-d in scope.
#[derive(Template)]
#[template(path = "queue_editor.html")]
pub struct QueueEditorPage {
    pub csrf_token: String,
    pub queue: Queue,
    pub queue_id: String,
    pub version: i64,
    pub items: Vec<QueueItemRow>,
    pub can_write: bool,
    /// Whether the viewer may skip (`control.write`, §6.10.1) — global control,
    /// independent of queue write permission.
    pub can_control: bool,
}

/// The authoritative item list (`partials/queue_list.html`), returned by every
/// mutation so the server — never the UI — is the source of truth.
#[derive(Template)]
#[template(path = "partials/queue_list.html")]
pub struct QueueListFragment {
    pub csrf_token: String,
    pub queue_id: String,
    pub version: i64,
    pub items: Vec<QueueItemRow>,
    pub can_write: bool,
}

/// Create-form fragment (`partials/queue_form.html`), re-rendered inline on a
/// `422` validation error.
#[derive(Template)]
#[template(path = "partials/queue_form.html")]
pub struct QueueFormFragment {
    pub csrf_token: String,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #21: shows & schedule (calendar UI)
// ---------------------------------------------------------------------------

/// One calendar column (a Berlin day) with its broadcast cards.
#[derive(Clone)]
pub struct CalendarCell {
    /// ISO date in Berlin (`2026-11-18`).
    pub date: String,
    pub day_code: String,
    pub day_num: String,
    pub in_month: bool,
    pub is_today: bool,
    pub entries: Vec<BroadcastCard>,
}

/// One calendar row (7 cells, Monday-first).
#[derive(Clone)]
pub struct CalendarWeek {
    pub cells: Vec<CalendarCell>,
}

/// A broadcast slot rendered in the calendar (open bar when `ends_at` is null).
#[derive(Clone)]
pub struct BroadcastCard {
    pub id: String,
    pub persisted: bool,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub caster_id: String,
    pub time_label: String,
    pub open_end: bool,
    pub warnings: Vec<WarningView>,
}

/// A soft conflict warning as rendered (D12 — never blocks).
#[derive(Clone)]
pub struct WarningView {
    pub code: String,
    pub message: String,
    pub severity: String,
}

/// The calendar grid fragment (`partials/calendar_grid.html`), also embedded in
/// the full page so `{% include %}`-d fields stay in scope.
#[derive(Template)]
#[template(path = "partials/calendar_grid.html")]
pub struct CalendarGridFragment {
    pub csrf_token: String,
    pub view: String,
    pub view_label: String,
    pub anchor: String,
    pub prev_anchor: String,
    pub next_anchor: String,
    pub title: String,
    pub weeks: Vec<CalendarWeek>,
    pub can_write: bool,
}

/// Schedule page (`GET /schedule`).
#[derive(Template)]
#[template(path = "schedule.html")]
pub struct SchedulePage {
    pub csrf_token: String,
    pub view: String,
    pub view_label: String,
    pub anchor: String,
    pub prev_anchor: String,
    pub next_anchor: String,
    pub title: String,
    pub weeks: Vec<CalendarWeek>,
    pub can_write: bool,
    /// Server-side inline error (e.g. a hard `show_overlap` reload).
    pub error: Option<String>,
}

/// One `<option>` of the show selector.
pub struct ShowOption {
    pub id: String,
    pub name: String,
}

/// One `<option>` of the queue selector.
pub struct QueueOption {
    pub id: String,
    pub name: String,
}

/// The broadcast form state (shared by page + fragment).
pub struct BroadcastForm {
    pub id: String,
    pub version: i64,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub show_id: String,
    /// Berlin wall-clock `datetime-local` values (`""` when unset).
    pub starts_at: String,
    pub ends_at: String,
    pub tape_id: String,
    pub queue_id: String,
    pub shows: Vec<ShowOption>,
    pub queues: Vec<QueueOption>,
}

/// Broadcast form page (`GET /schedule/new`, `GET /schedule/{id}/edit`).
#[derive(Template)]
#[template(path = "broadcast_form.html")]
pub struct BroadcastFormPage {
    pub csrf_token: String,
    pub form: BroadcastForm,
    pub warnings: Vec<WarningView>,
    pub error: Option<String>,
}

/// Broadcast form fragment (`HX-Request` → date change re-preview, `422`/`409`
/// re-render).
#[derive(Template)]
#[template(path = "partials/broadcast_form.html")]
pub struct BroadcastFormFragment {
    pub csrf_token: String,
    pub form: BroadcastForm,
    pub warnings: Vec<WarningView>,
    pub error: Option<String>,
}

/// A show plus the per-viewer capabilities.
pub struct ShowRow {
    pub show: Show,
    pub can_write: bool,
    pub is_own: bool,
    pub has_recurrence: bool,
}

/// Show list (`GET /shows`) + create form.
#[derive(Template)]
#[template(path = "shows.html")]
pub struct ShowsPage {
    pub csrf_token: String,
    pub can_write: bool,
    pub shows: Vec<ShowRow>,
    pub error: Option<String>,
}

/// Show detail (`GET /shows/{id}`) incl. edit form.
#[derive(Template)]
#[template(path = "show_detail.html")]
pub struct ShowDetailPage {
    pub csrf_token: String,
    pub show: Show,
    pub can_write: bool,
    pub has_recurrence: bool,
    /// The raw rule string (`""` when none) — precomputed for the form field.
    pub recurrence_text: String,
    pub error: Option<String>,
}
