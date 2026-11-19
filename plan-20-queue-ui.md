# Plan: Issue #20 — Queue-UI (Milestone M2 Portal)

- Repo: momokli/zukka_radio
- Basis-Branch: `v2` (Tip `7fb7262`), PR-Ziel: `v2`
- Feature-Branch: `feature/20-queue-ui`
- Scope: **nur `portal/`** (+ ggf. `docs/`). Keine `scheduler/`-Änderung, keine
  Migrationen, keine CI-/Config-/Compose-Änderung.
- Source of Truth: `docs/consolidated-architecture.md` (§2.6, §3.3, §3.4, §4.2),
  `docs/portal.md` (§2 D9/D10/D18, §3.2, §3.3, §3.5, §3.8, P4-DoD).
- Verifizierter Kontext: Scheduler hat Queue-CRUD vollständig (#10, gemergt)
  unter `/api/v1/queues*`. Portal folgt dem Muster
  `TrackApi`/`HttpTrackApi`/`FakeTrackApi` (`portal/src/api_client.rs`) →
  Bearer-Service-Token + `X-Caster-Id`. Portal ist thin UI, read-only DB.

---

## 0. Ausgangslage (verifiziert)

- `portal/src/api_client.rs` enthält bereits `IdentityApi`, `UploadJobApi`,
  `WaveformApi`, `TrackApi` samt Http-/Fake-Implementierungen und den
  Fehler-Mappings `TrackError` (404/409/422) und `JobError` (404/409/422).
- `portal/src/library.rs` zeigt das Handler-/Fragment-Muster:
  `is_htmx(&headers)` → Fragment statt Seite; `render<T: Template>`;
  `track_error()` mappt auf `PortalError`.
- `portal/src/app.rs` baut den `protected`-Router (CSRF- + Session-Layer) und
  `AppState` mit `Arc<dyn TrackApi>`; `AppState::with_clients(...)` für Tests.
- `portal/src/rbac.rs` führt `queue.write.own` (caster/operator/admin) und
  `queue.write.any` (operator/admin); `rbac::check` prüft **nur** Permissions.
- `portal/templates/layout.html` Nav: Dashboard / Library / Konto.
- Scheduler-Contract (real, aus `scheduler/src/dto.rs` + `store/queues.rs`):
  - `QueueDto { id, caster_id, name, description, kind(background|slot), version, created_at, updated_at }`
  - `QueueItemDto { id, queue_id, position, kind(track|tape|link), track_id?, tape_id?, link_url?, link_provider?(youtube|spotify|other), link_state?(pending|ready|failed|unplayable), title_override?, state(pending|playing|played|skipped), played_at?, created_at }`
  - `POST /queues { caster_id, name, description?, kind? }` → 201
  - `GET /queues?caster_id=&kind=` → 200 Liste
  - `GET/PATCH/DELETE /queues/{id}`; PATCH/DELETE **verlangen `version`**
    (optimistic lock) → `409 version_conflict`
  - `GET/POST /queues/{id}/items`; `POST` Body `{ kind, track_id|tape_id|link_url, link_provider?, title_override?, position? }`
  - `PATCH/DELETE /queues/{id}/items/{item_id}` (Content-Felder unveränderlich)
  - `POST /queues/{id}/reorder { item_ids:[...], version }` → `QueueDto`;
    validiert exakte Item-Menge (`422 item_ids`), `409 version_conflict`
  - `POST /queues/{id}/clear` → 204
- **Kein Queue-Skip im Scheduler.** Skip läuft global über
  `POST /api/v1/control/skip { reason }` (skippt das aktuell laufende Item,
  kein Queue-Bezug).

---

## 1. Betroffene Dateien

| Datei | neu/geändert | Inhalt |
| ----- | ------------ | ------ |
| `portal/src/api_client.rs` | geändert | `Queue`/`QueueItem` DTOs, `QueueError`, `QueueApi`-Trait, `HttpQueueApi`, `FakeQueueApi`, `ControlApi`-Trait (skip) + `HttpControlApi`/`FakeControlApi` |
| `portal/src/queues.rs` | **neu** | Handler + Fragment-Erkennung + Ownership/RBAC-Helfer |
| `portal/src/templates.rs` | geändert | Page-/Fragment-Structs (`QueuesPage`, `QueueEditorPage`, `QueueListFragment`, `QueueItemFragment`, `QueueFormFragment`) |
| `portal/templates/queues.html` | **neu** | Queue-Übersicht (eigene + fremde lesbar) + Anlege-Formular |
| `portal/templates/queue_editor.html` | **neu** | Queue-Editor: Items, Add-Formular (Track/Link), Rename, Skip, Clear |
| `portal/templates/partials/queue_list.html` | **neu** | `<ul>` der Items + SortableJS-Hook + no-JS ↑/↓ |
| `portal/templates/partials/queue_item.html` | **neu** | eine Item-Zeile inkl. Link-Status (`link_state`) |
| `portal/templates/partials/queue_form.html` | **neu** | Anlege-Formular (Fragment nach `POST /queues`) |
| `portal/templates/layout.html` | geändert | Nav-Link „Queues" |
| `portal/static/sortable.min.js` | **neu** | vendored SortableJS (kein CDN, D18) |
| `portal/src/app.rs` | geändert | `AppState.queues: Arc<dyn QueueApi>` + `AppState.control: Arc<dyn ControlApi>`, Default-Clients, Routen registrieren, `/static/sortable.min.js` serven |
| `portal/src/lib.rs` | geändert | `pub mod queues;` |
| `portal/tests/queue_ui_e2e.rs` | **neu** | Portal-Handler gegen `FakeQueueApi`/`FakeControlApi` |
| `portal/tests/queue_scheduler_e2e.rs` | **neu** | `HttpQueueApi` gegen echten Scheduler (ephemerer Port, Muster `library_scheduler_e2e.rs`) |
| `docs/portal.md` | optional | P4-DoD-Häkchen / Klarstellung Skip-Semantik (nur wenn nötig) |

`plan-*.md` / `progress-*.md` bleiben **untracked**.

---

## 2. User Stories + Akzeptanzkriterien

### US-1 — Queue-Übersicht (`GET /queues`)
- Zeigt **eigene** Queues (Name, Kind, Item-Anzahl, `version`) und **fremde**
  Queues read-only (D9: „eigene + fremde lesbar").
- Permission: **nur login** (jeder angemeldete Caster).
- Leerzustand „noch keine Queue" (`partials/empty_state.html`).
- AK: Fremde Queue erscheint in der Liste, aber ohne Schreib-Aktionen.

### US-2 — Queue anlegen (`POST /queues`)
- Formular: `name` (Pflicht), `description` (optional), `kind`
  (`background`|`slot`, Default `background`).
- Permission: `queue.write.own`.
- **Ownership:** nur `queue.write.own` → `caster_id` wird serverseitig auf
  `ctx.caster_id` gesetzt (Client-`caster_id` wird ignoriert); nur
  `queue.write.any` → `caster_id` aus dem Formular erlaubt.
- AK: Anlegen erzeugt Queue mit `caster_id == ctx.caster_id`; leerer Name → 422
  inline gerendert.

### US-3 — Queue-Editor (`GET /queues/{id}`)
- Zeigt Items in Positionsreihenfolge, Add-Formular, Rename, Skip, Clear.
- Permission: **nur login** (fremde Queue read-only, keine Schreib-Buttons).
- `can_write` wird serverseitig berechnet (siehe §4), nie aus Client-Input.
- AK: Fremd-Queue öffnet sich, Schreib-Aktionen fehlen bzw. sind disabled.

### US-4 — Item hinzufügen (`POST /queues/{id}/items`)
- Track hinzufügen: `kind=track` + `track_id` (aus Library-Kontext).
- Link hinzufügen: `kind=link` + `link_provider` (youtube|spotify|other) +
  `link_url` (D10).
- Permission: `queue.write.own`/`.any` (+ Ownership, §4).
- AK: neues Item erscheint am Ende (`position`), htmx tauscht `queue-list`.

### US-5 — Umsortieren (`POST /queues/{id}/items/{item}/move`)
- Body: `before_id` **oder** `after_id` (+ optional `version`).
- Portal baut daraus die **vollständige** `item_ids`-Liste und ruft
  `POST /queues/{id}/reorder { item_ids, version }` auf (§5).
- Permission: `queue.write.own`/`.any` (+ Ownership).
- AK: Drag&Drop und ↑/↓-Button erzeugen **denselben** DB-Zustand; Reihenfolge
  überlebt Reload (Version, §5).

### US-6 — Item entfernen (`POST /queues/{id}/items/{item}/delete`)
- ruft `DELETE /queues/{id}/items/{item_id}`.
- Permission: `queue.write.own`/`.any` (+ Ownership).
- AK: `hx-confirm` beim Löschen; Fragment aktualisiert.

### US-7 — Queue umbenennen (`POST /queues/{id}/rename`)
- Body: `name` (+ `description?` + `version`).
- ruft `PATCH /queues/{id}` mit `version`.
- Permission: `queue.write.own`/`.any` (+ Ownership).
- AK: `409 version_conflict` → Fragment neu laden + Toast (§5).

### US-8 — Skip (`POST /queues/{id}/skip`)
- Portal ruft **`POST /api/v1/control/skip { reason }`** (§6). Kein
  Scheduler-Queue-Skip.
- Permission: `queue.write.own`/`.any` (+ Ownership).
- AK: Läuft die Queue nicht als aktive Quelle → **klare Meldung**, kein
  Fehler-Toast mit Stacktrace; Scheduler-Fehler (409) wird als Toast gemappt.

### US-9 — Queue löschen (`DELETE` via `POST /queues/{id}/delete`)
- Optional (portal.md listet es nicht, aber CRUD vollständig). Body: `version`.
- Permission: `queue.write.own`/`.any` (+ Ownership).
- AK: `409 version_conflict` → Toast + Reload.

> **Nicht in diesem Issue:** Queue als aktive Quelle schalten (Layer 60) — das
> ist Control-Panel-Territorium (P6) und Scheduler-Feature. Scope-Grenze §9.

---

## 3. `api_client.rs` — Signaturen + DTO-Skizzen

```rust
// DTOs (Serde-Mirror der Scheduler-DTOs)
pub struct Queue {
    pub id: String,
    pub caster_id: String,
    pub name: String,
    pub description: String,
    pub kind: String,        // "background" | "slot"
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}
pub struct QueueItem {
    pub id: String,
    pub queue_id: String,
    pub position: i64,
    pub kind: String,        // "track" | "tape" | "link"
    pub track_id: Option<String>,
    pub tape_id: Option<String>,
    pub link_url: Option<String>,
    pub link_provider: Option<String>,  // youtube | spotify | other
    pub link_state: Option<String>,     // pending | ready | failed | unplayable
    pub title_override: Option<String>,
    pub state: String,       // pending | playing | played | skipped
    pub played_at: Option<String>,
    pub created_at: String,
}
pub struct QueueQuery { pub caster_id: Option<String>, pub kind: Option<String> }
pub struct NewQueue { pub caster_id: String, pub name: String,
                      pub description: Option<String>, pub kind: Option<String> }
pub struct NewQueueItem { pub kind: String, pub track_id: Option<String>,
                          pub tape_id: Option<String>, pub link_url: Option<String>,
                          pub link_provider: Option<String>,
                          pub title_override: Option<String>, pub position: Option<i64> }

pub enum QueueError {
    NotFound,                                   // 404
    Conflict(String),                           // 409 version_conflict
    Validation { field: String, message: String }, // 422
    Upstream(String),                           // sonst
}
impl QueueError { pub fn is_not_found(&self) -> bool; pub fn is_conflict(&self) -> bool; }

#[async_trait]
pub trait QueueApi: Send + Sync {
    async fn list(&self, viewer: Option<&str>, q: &QueueQuery) -> Result<Vec<Queue>, QueueError>;
    async fn get(&self, viewer: Option<&str>, id: &str) -> Result<Queue, QueueError>;
    async fn create(&self, viewer: Option<&str>, req: &NewQueue) -> Result<Queue, QueueError>;
    async fn update(&self, viewer: Option<&str>, id: &str, name: Option<&str>,
                    description: Option<&str>, version: i64) -> Result<Queue, QueueError>;
    async fn delete(&self, viewer: Option<&str>, id: &str, version: i64) -> Result<(), QueueError>;
    async fn items(&self, viewer: Option<&str>, queue_id: &str) -> Result<Vec<QueueItem>, QueueError>;
    async fn add_item(&self, viewer: Option<&str>, queue_id: &str, req: &NewQueueItem)
        -> Result<QueueItem, QueueError>;
    async fn update_item(&self, viewer: Option<&str>, queue_id: &str, item_id: &str,
        title_override: Option<&str>, state: Option<&str>, position: Option<i64>)
        -> Result<QueueItem, QueueError>;
    async fn delete_item(&self, viewer: Option<&str>, queue_id: &str, item_id: &str)
        -> Result<(), QueueError>;
    async fn reorder(&self, viewer: Option<&str>, queue_id: &str,
                     item_ids: &[String], version: i64) -> Result<Queue, QueueError>;
    async fn clear(&self, viewer: Option<&str>, queue_id: &str) -> Result<(), QueueError>;
}

pub struct HttpQueueApi { client, base_url, service_token }
// url() => "{base}/api/v1{path}"; request() setzt Bearer + X-Caster-Id
// error_from(): 404→NotFound, 409→Conflict(msg), 422→Validation{field,msg}, sonst Upstream

pub struct FakeQueueApi { /* Mutex<Vec<Queue>>, Mutex<Vec<QueueItem>>, counter */ }
// erzwingt dieselbe Ownership-/Item-Regel wie der Scheduler (genau ein
// Content-Feld, position monoton, version-Inkrement bei Mutation)

// Skip (Control) — eigener, kleiner Trait
#[async_trait]
pub trait ControlApi: Send + Sync {
    async fn skip(&self, reason: &str) -> Result<(), ControlError>;
}
pub enum ControlError { Conflict(String), Upstream(String) } // 409/422 → Conflict
pub struct HttpControlApi { client, base_url, service_token } // POST /api/v1/control/skip
pub struct FakeControlApi { /* Aufzeichnung + konfigurierbarer Fehler */ }
```

Fehler-Mapping identisch zu `TrackError` (404/409/422); `409` liefert den
`error.message`-String (`"version_conflict"` / `"queue was modified by another
client"`).

---

## 4. RBAC + Ownership (verbindlich)

Handler prüfen **zuerst** Permissions (`rbac::check`), **dann** Ownership.
Ownership wird serverseitig aus `Queue.caster_id` vs. `ctx.caster_id`
berechnet — **nie** aus Client-Input.

| Handler | Permission | Ownership-Regel |
| ------- | ---------- | --------------- |
| `GET /queues` (Liste) | **login** (keine) | Liste: eigene + fremde (read) |
| `POST /queues` | `queue.write.own` | `.own`-only → `caster_id = ctx.caster_id` (Body ignoriert); mit `.any` → Body-`caster_id` erlaubt |
| `GET /queues/{id}` | **login** (keine) | read jeder Queue |
| `POST /queues/{id}/items` | `queue.write.own` **oder** `queue.write.any` | `.any` → jede Queue; sonst nur `queue.caster_id == ctx.caster_id`, sonst **404** |
| `POST …/items/{item}/move` | s. o. | s. o. |
| `POST …/items/{item}/delete` | s. o. | s. o. |
| `POST /queues/{id}/rename` | s. o. | s. o. |
| `POST /queues/{id}/delete` | s. o. | s. o. |
| `POST /queues/{id}/skip` | s. o. | s. o. |

**Entscheidung:** eine fremde Queue bei nur `.own` antwortet **404** (nicht
403) — konsistent mit der Library-§1-Sichtbarkeit (fremd = unsichtbar). Fehlt
die Permission ganz (`.own` und `.any` beide false), → **403**
(`rbac::check`).

Hilfsfunktion in `queues.rs`:

```rust
// Prüft Permission + Ownership; Ok(()) nur wenn der Viewer schreiben darf.
async fn authorize_write(state: &AppState, ctx: &AuthContext, queue_id: &str)
    -> Result<Queue, PortalError>;
//  - ctx.has("queue.write.any")  -> Queue laden, Ok
//  - ctx.has("queue.write.own")  -> Queue laden; caster_id != ctx.caster_id -> NotFound
//  - sonst                        -> Forbidden
```

`GET`-Handler setzen `can_write` analog (ohne Fehler, nur Bool fürs Template).

---

## 5. Reorder mit Version

**Portal-Move (`POST /queues/{id}/items/{item}/move`)**
1. Body `MoveForm { before_id: Option<String>, after_id: Option<String>,
   version: Option<i64> }`.
2. Queue + Items laden (`GET /queues/{id}`, `GET /queues/{id}/items`).
3. Neue Reihenfolge **serverseitig** berechnen: `item` aus der Liste entfernen,
   dann vor `before_id` bzw. nach `after_id` einfügen. Keine Client-Reihenfolge
   vertrauen.
4. `item_ids` = vollständige, geordnete ID-Liste aller Items.
5. `POST /queues/{id}/reorder { item_ids, version }` mit `version` aus der
   zuletzt geladenen Queue.
6. htmx → `queue-list`-Fragment (frisch vom Scheduler) + `HX-Trigger: toast`.

**Verhalten bei `409 version_conflict` (D18/§5 Risiko „htmx + DnD-Zustandsdrift"):**
- Server liefert **nicht** die eigene Reihenfolge, sondern lädt Queue+Items neu
  und rendert das autoritative `queue-list`-Fragment.
- Zusätzlich `HX-Trigger: {"toast": {"level":"error",
  "message":"Queue wurde zwischenzeitlich geändert — neu geladen."}}`.
- Damit gilt: **Server ist immer die Wahrheit** (D9); die UI ist nie führend.

**SortableJS-Anbindung:** `queue_list.html` bekommt
`data-queue-id`/`data-version`; nach `onEnd` liest ein kleiner Inline-Handler
(`static/`-JS oder `hx-post` via Sortable-Hook) die Nachbar-IDs und POSTet
`before_id`/`after_id` + `version`. **No-JS:** ↑/↓-Buttons pro Zeile POSTen
denselben `move`-Endpunkt (D18, DoD „per Button derselbe DB-Zustand").

---

## 6. Skip-Semantik (Scope-Abgrenzung)

- Scheduler hat **keinen** Queue-Skip. Portal-`POST /queues/{id}/skip` ruft
  **`POST /api/v1/control/skip { reason }`** auf (globaler Skip des laufenden
  Items).
- Rückmeldung:
  - `200` → Toast „Skip gesendet."
  - `409` (z. B. „nichts läuft" / Queue nicht aktive Quelle) → Toast
    „Diese Queue ist gerade nicht die aktive Quelle — Skip wirkt auf das
    laufende Programm." (**klare Meldung statt Fehler**).
  - Transportfehler → Toast „Scheduler nicht erreichbar."
- **Kein neues Scheduler-Feature.** Portal kann nicht prüfen, ob *diese* Queue
  die aktive Quelle ist (kein Endpunkt dafür); die Formulierung ist bewusst
  neutral.
- AK aus portal.md P4: „Skip wirkt nur, wenn die Queue die aktive Quelle ist;
  sonst klare Rückmeldung." — erfüllt durch 409-Mapping + Text.

---

## 7. Links als Queue-Items (D10)

- Anlegen: `kind=link` mit `link_provider` (`youtube|spotify|other`) +
  `link_url`; `link_state` setzt der Scheduler (initial `pending`).
- `queue_item.html` rendert pro `link_state`:
  - `pending` → „wird aufgelöst …" (neutral)
  - `ready` → Provider-Label + Link
  - `failed` → „nicht auflösbar" + Grund-Hinweis
  - `unplayable` → **explizit** „Spotify: nicht abspielbar (Merkzettel)"
    (D10, AK „Spotify sichtbar als nicht abspielbar markiert").
- Link-Status wird **nicht** im Portal berechnet; er kommt aus `link_state`.
- Kein Polling in diesem Issue nötig: `queue-list`-Fragment refresht nach
  Mutation; optional `hx-trigger="every 5s"` auf der Liste (portal.md §3.3
  `queue-list`).

---

## 8. Test-Plan

### 8.1 `portal/tests/queue_ui_e2e.rs` (Portal-Handler vs. Fakes)
Muster: `library_e2e.rs` (Router + Cookie/CSRF-Login, `FakeIdentity`).
- **US-1/US-3:** Liste zeigt eigene + fremde Queues; Fremd-Queue read-only
  (keine Schreib-Buttons im HTML).
- **US-2:** `POST /queues` als caster → `caster_id == ctx.caster_id` (Body
  ignoriert); leerer Name → 422-Fragment.
- **US-4:** Track- und Link-Item anlegen (`kind=link` mit Provider/URL).
- **US-5 (Kern):** Move via `before_id`/`after_id` → `reorder` erhält
  vollständige `item_ids`; htmx-Fragment = autoritative Liste.
- **US-7/9:** Rename/Delete mit `version`; simulierter `409` → Fehler-Toast
  (`HX-Trigger`) + Reload-Fragment.
- **US-8:** Skip ruft `FakeControlApi`; 409 → freundlicher Toast.
- **RBAC/Ownership (Unit-nah):** `authorize_write`:
  - caster (nur `.own`) auf fremder Queue → `NotFound`;
  - operator (`.any`) auf fremder Queue → Ok;
  - Rolle ohne beide Permissions → `Forbidden`.
- **Reorder überlebt Reload:** nach Move erneut `GET /queues/{id}` und Items
  vergleichen (Version inkrementiert).

### 8.2 `portal/tests/queue_scheduler_e2e.rs` (echter Scheduler)
Muster: `library_scheduler_e2e.rs` (`zukka_scheduler::api::router` auf
`127.0.0.1:0`, migrierte Temp-DB, `HttpQueueApi`).
- **CRUD-Roundtrip:** create → list → add items (track + link) → reorder →
  get → delete.
- **Reorder + Version-Beweis:** reorder mit korrekter Version gelingt
  (Reihenfolge persistiert nach Neuladen); reorder mit **stale** `version` →
  `QueueError::Conflict` (`409 version_conflict`).
- **Fehler-Mapping:** `422` (leerer Name / `item_ids` falsche Menge) →
  `Validation`; `404` (unbekannte Queue/Item) → `NotFound`.
- **Fremd-Queue-Scope:** `queue.write.own`-Semantik wird im Portal geprüft
  (Scheduler-API ist aktuell noch ohne RBAC, `TODO(auth)`) — daher prüft dieser
  Test den **Datenpfad** (fremde Queue ist via `GET /queues?caster_id=` sichtbar
  und reorderbar), die Ownership-Grenze selbst in 8.1.
- **Skip:** `HttpControlApi::skip` gegen echten Scheduler → `200`/`409`-Mapping.

### 8.3 Unit-Tests (in `queues.rs` / `api_client.rs`)
- `FakeQueueApi`: genau-ein-Content-Feld-Invariante; `version`-Inkrement;
  Item-Menge beim `reorder` validiert (Mismatch → `Validation`).
- Move-Berechnung: vor erstem / nach letztem Item, unbekannte `before_id`/
  `after_id` → `BadRequest`.

**Beweise (explizit gefordert):** (a) Reorder überlebt Reload trotz
Versions-Guard, (b) Fremd-Queue ist für `.own`-only unsichtbar (404).

---

## 9. Risiken / Offene Punkte

| # | Punkt | Bewertung / Umgang |
| - | ----- | ------------------ |
| R1 | **Scheduler-Queue-API hat noch kein RBAC** (`TODO(auth)`, `scheduler/src/api/queues.rs`) | Portal prüft Ownership zusätzlich (§4); Scheduler-Boundary bleibt offen → Hinweis, **kein** Scheduler-Fix in diesem Issue. Ggf. Issue im Scheduler-Repo. |
| R2 | **Kein Endpunkt „ist diese Queue aktiv?"** | Skip-Rückmeldung neutral formuliert (§6); Scope-Grenze dokumentiert. |
| R3 | **htmx + DnD-Drift** | Server liefert nach jeder Mutation das autoritative Fragment (§5). |
| R4 | **`409` bei Rename/Delete/Reorder** | Einheitliches Muster: Reload-Fragment + `HX-Trigger`-Toast. |
| R5 | **`kind=slot`** ist broadcast-gebunden | Portal erlaubt Anlegen nur mit `kind=background` als Default; `slot`-Queues read-only bzw. nur von `.any` beschreibbar (Entscheidung im Editor-UI dokumentieren). |
| R6 | **SortableJS vendored** | Größe/Versions-Pin dokumentieren; kein CDN (D18). |
| R7 | **askama-Structs ohne Default** | `QueueItem`-Felder sind alle `Option`/String → template-sicher. |
| R8 | **`/queues/{id}/skip` Doppel-Semantik** | Route existiert (portal.md §3.2), ruft aber globales `control/skip`; im UI als „Skip (laufendes Programm)" beschriften. |

### Offene Punkte (im Issue/PR zu klären)
- O1: Dürfen `caster` fremde Queues in der Liste sehen (D9 ja) — aber sollen sie
  im Editor auch Link-Status sehen? → Vorschlag: ja, read-only.
- O2: Queue-Löschen als Route (`/queues/{id}/delete`) ist in portal.md §3.2
  nicht gelistet; Aufnahme als sinnvolle CRUD-Vervollständigung (US-9) oder
  streichen? → Vorschlag: aufnehmen, `queue.write.*`-gated.
- O3: `title_override` editierbar machen? → Vorschlag: nein (Scope klein halten).

---

## 10. DoD-Checkliste (docs/portal.md P4 — Queues)

- [ ] Queue-CRUD (anlegen, umbenennen, löschen) über Scheduler-API.
- [ ] Items: Track hinzufügen, Link hinzufügen (Provider + URL).
- [ ] Umsortieren per Drag&Drop (SortableJS) **und** ↑/↓-Buttons.
- [ ] Drag&Drop und ↑/↓ führen zum **selben** DB-Zustand.
- [ ] **Reihenfolge überlebt Reload** (Version/optimistic lock, §5).
- [ ] **Skip wirkt nur auf die aktive Quelle; sonst klare Rückmeldung** (§6).
- [ ] YouTube-Link `pending` → `ready`/`failed` ohne Blockieren (nur Anzeige).
- [ ] Spotify-Link sichtbar als „nicht abspielbar" markiert (D10).
- [ ] Fremde Queue read-only; `.own`-only → 404, `.any` → erlaubt (§4).
- [ ] Jede Seite/Fragment hat definierten Leerzustand + Fehlerdarstellung (D20).
- [ ] `cargo fmt --all -- --check`, `cargo clippy -- -D warnings`, `cargo test`
      grün (CI-Pflicht).

---

## 11. Implementierungs-Reihenfolge

1. **`api_client.rs`**: DTOs + `QueueError` + `QueueApi`/`HttpQueueApi` +
   `FakeQueueApi` + `ControlApi`/`HttpControlApi`/`FakeControlApi` (testbar in
   Isolation).
2. **`app.rs`**: `AppState.queues`/`AppState.control`, Default-Clients,
   `with_clients` erweitern (bestehende Tests anpassen).
3. **`queues.rs`**: Ownership-Helfer `authorize_write` + Handler + `is_htmx`.
4. **`templates.rs` + Templates**: `queues.html`, `queue_editor.html`,
   Partials, Nav-Link.
5. **`static/sortable.min.js`** + Sortable-Hook + ↑/↓-Buttons.
6. **Routen** in `app.rs` registrieren (`protected`), `/static/sortable.min.js`
   serven.
7. **Tests**: `queue_ui_e2e.rs`, `queue_scheduler_e2e.rs`, Unit-Tests.
8. `cargo fmt --all` → clippy → test → Branch pushen → PR gegen `v2`
   (`Closes #20`).

---

## 12. Scope-Grenzen (hart)

- **Nur `portal/`** (+ optional `docs/portal.md`).
- **Keine** `scheduler/`-Änderung (Queue-API, RBAC-TODO, Skip bleiben unberührt).
- **Keine** Migrationen, keine CI-/Config-/Compose-Änderung.
- `plan-*.md` / `progress-*.md` bleiben **untracked**.
