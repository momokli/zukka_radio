//! Askama template structs (one per page).

use askama::Template;

use crate::api_client::{
    Announcement, AnnouncementTemplate, CasterDto, ConnectedCaster, ControlState, Queue, QueueItem,
    Show, Track, UploadJob,
};

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
    /// Per-viewer live indicator (D5), refreshed via SSE/polling.
    pub badge: LiveBadgeView,
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

// ---------------------------------------------------------------------------
// Issue #101: admin user management (`/admin/users`)
// ---------------------------------------------------------------------------

/// A select option with its selected state precomputed server-side (Askama
/// cannot compare borrowed values in the template).
pub struct SelectOption {
    pub value: String,
    pub selected: bool,
}

/// One row of the admin table.
pub struct AdminUserRow {
    pub caster: CasterDto,
    pub is_self: bool,
    pub is_last_admin: bool,
    pub role_options: Vec<SelectOption>,
}

/// The one-time stream password shown after create/regenerate.
pub struct CreatedCasterView {
    pub username: String,
    pub role: String,
    pub harbor_mount: Option<String>,
    pub stream_password: Option<String>,
}

/// Inline field/error message.
pub struct FieldErrorView {
    pub field: String,
    pub message: String,
}

/// Repopulated create form values after a validation failure.
#[derive(Default)]
pub struct AdminUserFormValues {
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub role: String,
    pub harbor_mount: String,
}

/// Admin user page (`GET /admin/users`).
#[derive(Template)]
#[template(path = "admin_users.html")]
pub struct AdminUsersPage {
    pub csrf_token: String,
    pub casters: Vec<AdminUserRow>,
    pub roles: Vec<String>,
    pub created: Option<CreatedCasterView>,
    pub notice: Option<String>,
    pub error: Option<FieldErrorView>,
    pub form: AdminUserFormValues,
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
    pub anchor: String,
    pub prev_anchor: String,
    pub next_anchor: String,
    pub title: String,
    pub weeks: Vec<CalendarWeek>,
    /// `false` when the visible grid holds no entries (empty state).
    pub has_entries: bool,
    pub can_write: bool,
}

/// Schedule page (`GET /schedule`).
#[derive(Template)]
#[template(path = "schedule.html")]
pub struct SchedulePage {
    pub csrf_token: String,
    pub view: String,
    pub anchor: String,
    pub prev_anchor: String,
    pub next_anchor: String,
    pub title: String,
    pub weeks: Vec<CalendarWeek>,
    /// `false` when the visible grid holds no entries (empty state).
    pub has_entries: bool,
    pub can_write: bool,
    /// Server-side inline error (e.g. a hard `show_overlap` reload).
    pub error: Option<String>,
}

/// One layer row rendered by the control panel.
pub struct ControlLayerRow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub active: bool,
}

/// View model shared by the control page and the `control-state` fragment.
pub struct ControlView {
    pub layers: Vec<ControlLayerRow>,
    pub active_layer: Option<String>,
    pub paused: bool,
    pub derived: bool,
    pub live_override: Option<String>,
    pub live_active_caster: Option<String>,
    pub connected: Vec<ConnectedCaster>,
    pub has_connected: bool,
    pub now_title: Option<String>,
    pub now_artist: Option<String>,
    pub now_duration: String,
    /// Origin label of the now-playing item ("Datenbank" / "Icecast").
    pub now_source: Option<String>,
    /// Icecast reports audible output (Issue #140).
    pub on_air: bool,
    /// Machine-readable observation reason (`silent`, `icecast_unreachable`, …).
    pub observed_reason: Option<String>,
    /// "On Air" label derived from the **observed** signal (US7, #140).
    pub on_air_label: String,
    /// Optional note explaining the on-air label (e.g. "Icecast nicht erreichbar").
    pub on_air_note: Option<String>,
    /// Observed / derived layer plus a human-readable origin label (#140).
    pub observed_layer: Option<String>,
    pub derived_layer: Option<String>,
    pub layer_source: Option<String>,
    /// Whether the viewer may act (`control.write`); buttons hidden otherwise.
    pub can_write: bool,
    /// The scheduler was unreachable — render a hint instead of a `500` (D20).
    pub scheduler_error: bool,
}

impl ControlView {
    /// Builds the view from a scheduler state + the viewer's write capability.
    pub fn from_state(state: ControlState, can_write: bool) -> Self {
        let active_layer = state.active_layer.clone();
        let layers = state
            .sources
            .into_iter()
            .map(|source| ControlLayerRow {
                active: active_layer.as_deref() == Some(source.name.as_str()),
                id: source.id,
                name: source.name,
                enabled: source.enabled,
            })
            .collect();

        // DB now-playing wins; otherwise fall back to the Icecast observation
        // (relay/rotation never appear in the DB path — DD4, #140).
        let (now_title, now_artist, now_duration, now_source) = match state.now_playing {
            Some(item) => (
                Some(item.title),
                item.artist,
                format_duration(item.duration_ms),
                Some("Datenbank".to_string()),
            ),
            None => match state.observed_now_playing {
                Some(item) => (
                    item.title,
                    item.artist,
                    String::new(),
                    Some(item.source.unwrap_or_else(|| "Icecast".to_string())),
                ),
                None => (None, None, String::new(), None),
            },
        };

        let layer_source = state.active_layer_source.as_deref().map(|raw| match raw {
            "observed" => "beobachtet".to_string(),
            "derived" => "abgeleitet".to_string(),
            other => other.to_string(),
        });

        let has_connected = !state.live.connected.is_empty();

        // US7 (#140): render "On Air" from the *observed* signal, not the
        // effective layer — a derived layer must not hide an observed silence.
        let (on_air_label, on_air_note) = match state.observed_reason.as_deref() {
            Some("silent") => ("Stille".to_string(), None),
            Some("icecast_unreachable") => (
                "unbekannt".to_string(),
                Some("Icecast nicht erreichbar".to_string()),
            ),
            _ => match state.observed_layer.as_deref() {
                Some(layer) => (layer.to_string(), Some("beobachtet".to_string())),
                None if state.on_air => ("unbekannt".to_string(), None),
                None => ("Stille".to_string(), None),
            },
        };

        Self {
            layers,
            active_layer,
            paused: state.paused,
            derived: state.derived,
            live_override: state.live.override_caster_id,
            live_active_caster: state.live.active_caster_id,
            connected: state.live.connected,
            has_connected,
            now_title,
            now_artist,
            now_duration,
            now_source,
            on_air: state.on_air,
            observed_reason: state.observed_reason,
            on_air_label,
            on_air_note,
            observed_layer: state.observed_layer,
            derived_layer: state.derived_layer,
            layer_source,
            can_write,
            scheduler_error: false,
        }
    }

    /// A degraded view for an unreachable scheduler (D20: fragment, not `500`).
    pub fn unreachable(can_write: bool) -> Self {
        Self {
            layers: Vec::new(),
            active_layer: None,
            paused: false,
            derived: false,
            live_override: None,
            live_active_caster: None,
            connected: Vec::new(),
            has_connected: false,
            now_title: None,
            now_artist: None,
            now_duration: String::new(),
            now_source: None,
            on_air: false,
            observed_reason: None,
            on_air_label: "Stille".to_string(),
            on_air_note: None,
            observed_layer: None,
            derived_layer: None,
            layer_source: None,
            can_write,
            scheduler_error: true,
        }
    }
}

/// Formats milliseconds as `m:ss` (empty for `0`).
fn format_duration(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let total = ms / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Control panel page (`GET /control`).
#[derive(Template)]
#[template(path = "control.html")]
pub struct ControlPage {
    pub csrf_token: String,
    pub view: ControlView,
}

/// The `control-state` fragment (`GET /control/state`), swapped via SSE/polling.
#[derive(Template)]
#[template(path = "partials/control_state.html")]
pub struct ControlStateFragment {
    pub csrf_token: String,
    pub view: ControlView,
}

/// Per-viewer live indicator (`du bist live`), D5 — caster sees only its own.
pub struct LiveBadgeView {
    pub is_live: bool,
}

/// The `live-badge` fragment (`GET /control/badge`).
#[derive(Template)]
#[template(path = "partials/live_badge.html")]
pub struct LiveBadgeFragment {
    pub badge: LiveBadgeView,
}

/// A toast fragment (`partials/toast.html`), used for out-of-band swaps.
#[derive(Template)]
#[template(path = "partials/toast.html")]
pub struct ToastFragment {
    pub level: String,
    pub message: String,
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
    /// Lifecycle status of the broadcast being edited (`""` for a new form).
    pub status: String,
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
}

/// Inline error fragment for the show create/edit forms: an htmx `422`
/// re-renders this into the form's error slot (the form itself keeps its
/// entered values because nothing is swapped away).
#[derive(Template)]
#[template(path = "partials/show_form_error.html")]
pub struct ShowFormErrorFragment {
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Issue #38: manual announcements
// ---------------------------------------------------------------------------

/// A manual announcement plus its precomputed display label.
pub struct AnnouncementRow {
    pub announcement: Announcement,
    pub status_label: String,
}

impl From<Announcement> for AnnouncementRow {
    fn from(announcement: Announcement) -> Self {
        let status_label = match announcement.status.as_str() {
            "pending" => "ausstehend",
            "sent" => "gesendet",
            "failed" => "fehlgeschlagen",
            "cancelled" => "abgebrochen",
            other => other,
        }
        .to_string();
        Self {
            announcement,
            status_label,
        }
    }
}

/// Manual announcements page (`GET /announcements`): post form (when allowed)
/// + manual templates + the latest manual posts.
#[derive(Template)]
#[template(path = "announcements.html")]
pub struct AnnouncementsPage {
    pub csrf_token: String,
    pub can_post: bool,
    /// The session display name, pre-filled into the form.
    pub caster_name: String,
    pub templates: Vec<AnnouncementTemplate>,
    pub recent: Vec<AnnouncementRow>,
}
