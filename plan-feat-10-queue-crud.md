# Plan: Issue #10 — Queue-CRUD-API im `zukka-scheduler`

Basis: `v2` · Branch `feat/10-queue-crud` · Deliverable: gepushter Branch + offener PR mit `Closes #10`.
Source of Truth: `docs/consolidated-architecture.md` §3.3 (queues/queue_items), §3.4 (Enums),
§4.2 (Queues-Endpunkte), §4.6 (Fehler-Envelope), §6.5 C36 (Reorder); `docs/data-model.md` §2.6.
Schema `scheduler/migrations/0001_init.sql` ist **EINGEFROREN** — keine Migrationsänderung.

## Ziel

Lese-/Schreib-API für `queues` und `queue_items` unter `/api/v1`, mit optimistischer
Sperre über `queues.version` (409), Fehler-Envelope nach §4.6 und UUIDv7-IDs über
`zukka_scheduler::uuid::new_id()`.

## Endpunkt-Matrix (Soll)

| Methode  | Pfad                           | Erfolg | Fehler                     |
| -------- | ------------------------------ | ------ | -------------------------- |
| `GET`    | `/queues?caster_id=&kind=`     | `200`  | `400`                      |
| `POST`   | `/queues`                      | `201`  | `400`, `404`, `409`, `422` |
| `GET`    | `/queues/{id}`                 | `200`  | `404`                      |
| `PATCH`  | `/queues/{id}`                 | `200`  | `404`, `409`, `422`        |
| `DELETE` | `/queues/{id}`                 | `204`  | `404`, `409`               |
| `GET`    | `/queues/{id}/items`           | `200`  | `404`                      |
| `POST`   | `/queues/{id}/items`           | `201`  | `404`, `409`, `422`        |
| `PATCH`  | `/queues/{id}/items/{item_id}` | `200`  | `404`, `422`               |
| `DELETE` | `/queues/{id}/items/{item_id}` | `204`  | `404`                      |
| `POST`   | `/queues/{id}/reorder`         | `200`  | `404`, `409`, `422`        |
| `POST`   | `/queues/{id}/clear`           | `204`  | `404`                      |

Axum-0.8-Paramsyntax ist `{id}` (nicht `:id`). Auth/RBAC ist **nicht** Teil dieses Issues
(nur der Router + Handler); Middleware kommt in einem separaten Issue — im Plan als
`TODO(auth)` markieren, aber keine Auth-Logik implementieren.

## Mapping auf das eingefrorene Schema (keine Schemaänderung)

`queues`: `id, caster_id, name, description, kind(background|slot), version, created_at, updated_at`,
`UNIQUE (caster_id, name)`.
`queue_items`: `id, queue_id, position, kind(track|tape|link), track_id, tape_id, link_url,
link_provider(youtube|spotify|other), link_state(pending|ready|failed|unplayable), title_override,
state(pending|playing|played|skipped), played_at, created_at`, `UNIQUE (queue_id, position)`.

Wichtig: Die DTO-Tabelle in `data-model.md` §3.4 ist teils veraltet (INTEGER-IDs, kein
`link_*`). **Das Schema gewinnt.** DTO-Felder müssen `link_url`, `link_provider`, `link_state`
enthalten; die CHECK-Matrix „genau ein Inhalt" wird sauber in Rust abgebildet.

## Betroffene Dateien

- `scheduler/src/error.rs` (neu) — `ApiError` + Fehler-Envelope (§4.6).
- `scheduler/src/dto.rs` (neu) — Request/Response-DTOs + Enum-Typen (`QueueKind`,
  `QueueItemKind`, `QueueItemState`, `LinkProvider`, `LinkState`) mit
  `#[serde(rename_all="snake_case")]` + `sqlx::Type`.
- `scheduler/src/store/mod.rs`, `store/queues.rs`, `store/queue_items.rs` (neu) — sqlx-Queries.
- `scheduler/src/api/mod.rs`, `api/queues.rs` (neu) — Axum-Handler.
- `scheduler/src/lib.rs` — `pub mod error; pub mod dto; pub mod store; pub mod api;`
  + `pub fn app(state: AppState) -> Router` (testbarer Router-Builder).
- `scheduler/src/main.rs` — `let app = zukka_scheduler::api::router(pool);` statt Inline-Router.
- `scheduler/tests/queue_crud.rs` (neu) — Integrationstests über den Router.
- `scheduler/Cargo.toml` — **Dev-Dependencies ergänzen** für HTTP-Tests ohne Netz:
  `tower = { version = "0.5", features = ["util"] }`, `http-body-util = "0.1"`,
  `mime = "0.3"`. Keine neuen Runtime-Dependencies (axum/sqlx/uuid/serde_json vorhanden).

## Fehler-Envelope (§4.6)

```json
{ "error": { "code": "validation_error", "message": "…", "details": { "field": "…" } } }
```

`ApiError`-Enum (thiserror) + `IntoResponse`:
- `NotFound { resource, id }` → `404`, code `not_found`.
- `Conflict { code, message, details }` → `409`; codes: `version_conflict`, `duplicate_name`,
  `duplicate_position`, `queue_in_use`.
- `Validation { field, reason }` → `422`, code `validation_error`.
- `BadRequest { message }` → `400`, code `bad_request`.
- `Internal(anyhow::Error)` → `500`, code `internal_error` (Details **nicht** nach außen leaken).
Serialisierung über `serde_json::json!` in den festen Envelope.

## Optimistische Sperre — Design-Entscheidung

- `queues.version` startet bei `1`, `CHECK (version >= 1)` (DB).
- `PATCH /queues/{id}` und `POST /queues/{id}/reorder` **verlangen** `version` im Body.
  Update läuft als bedingtes `UPDATE … SET version = version + 1, … WHERE id = ? AND version = ?`;
  betrifft 0 Zeilen → unterscheidet im Anschluss „existiert nicht" (`404`) vs. „stale" (`409`).
- Antwort DTO gibt die **neue** `version` zurück.
- Item-Mutationen (`POST`/`PATCH`/`DELETE` items, `clear`) erhöhen ebenfalls `queues.version`
  (Inhalt == Zustand), verlangen aber laut Endpunkt-Matrix **keine** Client-Version.
  Reorder ist der einzige Item-Endpunkt mit Versionspflicht (Reorder ist reihenfolgekritisch).
- Reorder ist **transaktional**: `item_ids` muss exakt die aktuellen Item-IDs der Queue sein
  (gleiche Menge), sonst `422` (`details.field = "item_ids"`); `position` wird auf `0..n-1`
  neu geschrieben. Zwischenschritt: `position` temporär auf `position + OFFSET` verschieben,
  um die `UNIQUE (queue_id, position)`-Kollision während des Rewrites zu vermeiden.

## User Stories (Reihenfolge = Implementierungsreihenfolge)

**US1 — API-Gerüst + Fehler-Envelope + Router-Builder**
- Akzeptanz: `error.rs`, `dto.rs` (Enums + DTOs) existieren; `api::router(pool) -> Router<()>`
  mountet `/api/v1`; `main.rs` nutzt ihn; `/healthz` bleibt.
- `GET /api/v1/queues` unbekannte Route → `404` mit Envelope, nicht Axum-Default-Text.
- Betroffen: `error.rs`, `dto.rs`, `api/mod.rs`, `lib.rs`, `main.rs` · ~120 LOC.

**US2 — Queue lesen (`GET /queues`, `GET /queues/{id}`)**
- Akzeptanz: Liste filterbar nach `caster_id` und `kind` (beide optional), sortiert
  `created_at, id`; Detail `200` mit allen DTO-Feldern, fehlend → `404 not_found`.
- Betroffen: `store/queues.rs`, `api/queues.rs` · ~70 LOC.

**US3 — Queue anlegen (`POST /queues`)**
- Akzeptanz: Body `{caster_id, name, description?, kind?}`; `id = new_id()`, `version = 1`,
  `created_at == updated_at` (RFC3339 UTC ms). Validierung: `name` nicht leer, `kind` ∈
  `background|slot` (Default `background`) → sonst `422`. Doppelter `(caster_id, name)` → `409
  duplicate_name`. Unbekannter `caster_id` (FK) → `404` bzw. `422` (FK-Fehler mappen).
- Betroffen: `store/queues.rs`, `api/queues.rs` · ~60 LOC.

**US4 — Queue ändern (`PATCH /queues/{id}`) mit Version**
- Akzeptanz: Body `{name?, description?, kind?, version}`; `version` Pflicht. Erfolgreich →
  `200` mit `version+1`. `WHERE id AND version` trifft 0 → `409 version_conflict` (wenn id
  existiert) bzw. `404`. Namensänderung auf existierenden Namen → `409 duplicate_name`.
- Betroffen: `store/queues.rs`, `api/queues.rs` · ~60 LOC.

**US5 — Queue löschen (`DELETE /queues/{id}`)**
- Akzeptanz: `204`. `queue_items` kaskadieren (DB-FK `ON DELETE CASCADE`). Ist die Queue von
  einem `playlist`-Broadcast referenziert (`broadcasts.queue_id ON DELETE SET NULL` → kein FK-Fehler),
  reicht `204`; ein explizites „in use"-Gate ist optional und in `store` als Kommentar zu markieren.
- Betroffen: `store/queues.rs`, `api/queues.rs` · ~25 LOC.

**US6 — Items lesen + hinzufügen (`GET`/`POST /queues/{id}/items`)**
- Akzeptanz: `GET` liefert Items sortiert `position, id`. `POST` Body
  `{kind, track_id?, tape_id?, link_url?, link_provider?, title_override?, position?}`:
  genau-ein-Inhalt je `kind` validieren (`track`↔`track_id`, `tape`↔`tape_id`, `link`↔`link_url`)
  → sonst `422 validation_error` (`details.field`). Ohne `position` → ans Ende (`MAX(position)+1`).
  Kollidierende `position` → `409 duplicate_position`. Antwort `201` mit `QueueItemDto`.
- Betroffen: `store/queue_items.rs`, `api/queues.rs` · ~90 LOC.

**US7 — Item ändern/löschen (`PATCH`/`DELETE /queues/{id}/items/{item_id}`)**
- Akzeptanz: `PATCH` Body `{title_override?, state?, position?}` (Content-Felder immutable);
  `state` ∈ `pending|playing|played|skipped`; Position wechselt konfliktfrei → sonst `422`.
  Fehlendes Item → `404`. `DELETE` → `204`; danach Lücke in `position` erlaubt (kein Reindex).
- Betroffen: `store/queue_items.rs`, `api/queues.rs` · ~70 LOC.

**US8 — Reorder (`POST /queues/{id}/reorder`)**
- Akzeptanz: Body `{item_ids: [uuid…], version: u32}`. Transaktion:
  (1) `SELECT version` und Item-Menge prüfen; `version` mismatch → `409 version_conflict`;
  (2) `item_ids`-Menge ≠ aktuelle Menge (Duplikat/fehlend/fremd) → `422`;
  (3) Positionen `0..n-1` schreiben; `version = version + 1`; Commit. Antwort `200` mit Queue-DTO.
- Betroffen: `store/queues.rs`, `api/queues.rs` · ~80 LOC.

**US9 — Clear (`POST /queues/{id}/clear`)**
- Akzeptanz: `DELETE FROM queue_items WHERE queue_id = ?` + `UPDATE queues SET version =
  version + 1, updated_at = ?`; Antwort `204`. Unbekannte Queue → `404`.
- Betroffen: `store/queues.rs`, `api/queues.rs` · ~25 LOC.

**US10 — Integrationstests**
- Akzeptanz: siehe Teststrategie; `cargo test -p zukka-scheduler` grün.
- Betroffen: `scheduler/tests/queue_crud.rs` · ~300 LOC.

## Axum-Router-Struktur

```rust
pub fn router(pool: SqlitePool) -> Router {
    let api = Router::new()
        .route("/queues", get(list_queues).post(create_queue))
        .route("/queues/{id}", get(get_queue).patch(patch_queue).delete(delete_queue))
        .route("/queues/{id}/items", get(list_items).post(add_item))
        .route("/queues/{id}/items/{item_id}", patch(patch_item).delete(delete_item))
        .route("/queues/{id}/reorder", post(reorder))
        .route("/queues/{id}/clear", post(clear));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", api)
        .fallback(not_found)
        .with_state(pool)
}
```

## Teststrategie (Integration über den Router, keine Netz-Sockets)

Testhelfer in `scheduler/tests/queue_crud.rs` (analog `schema_invariants.rs`):
`setup()` = `TempDir` + `db::connect` (führt Migration aus); `app()` = `api::router(pool)`;
`send(app, method, uri, json)` über `tower::ServiceExt::oneshot` +
`axum::body::to_bytes`, Body via `serde_json::from_slice`.

1. **Queue CRUD happy path**: POST → 201 (`version==1`, UUIDv7 = 36 Zeichen) → GET Liste
   enthält Queue → GET Detail → PATCH (version 1) → 200 (`version==2`) → DELETE → 204 →
   GET Detail → 404.
2. **Version-Konflikt**: PATCH mit `version=1` nach bereits erfolgtem PATCH → `409
   version_conflict`; Reorder mit falscher `version` → `409`.
3. **Validierung**: POST Queue leerer `name` → 422; `kind="bogus"` → 422; POST Item mit
   `track_id` **und** `tape_id` → 422; `kind=link` ohne `link_url` → 422.
4. **Duplikate**: zwei Queues gleicher `(caster_id,name)` → 409; zwei Items gleicher
   `position` → 409.
5. **Items**: track/tape/link anlegen, `GET resources` Reihenfolge == `position`; PATCH
   `state`/`title_override`; DELETE → 204.
6. **Reorder**: 3 Items, `item_ids` verdreht + korrekte `version` → 200, `GET` liefert neue
   Reihenfolge, `position ∈ {0,1,2}`; `item_ids` mit fehlendem/doppeltem ID → 422.
7. **Clear**: Items anlegen → clear → 204 → `GET items` leer; `version` erhöht.
8. **404**: alle `{id}`-Endpunkte mit unbekannter UUID → `404` + Envelope-Form
   (`error.code == "not_found"`).

Idempotenz-/Pagination-Header (`Idempotency-Key`, `limit/offset`) sind **out of scope**
für #10 (kein Acceptance-Kriterium) — als Kommentar vermerken.

## Implementierungs-Reihenfolge

US1 → US2 → US3 → US4 → US5 → US6 → US7 → US8 → US9 → US10.
Nach jedem Block: `cargo fmt --all`, `cargo clippy -p zukka-scheduler --all-targets -- -D warnings`,
`cargo test -p zukka-scheduler`. **Kein** `sqlx::query!`-Makro (Offline-CI ohne DB) —
Laufzeit-`sqlx::query`/`query_as` mit `bind` verwenden.

## Scope-Guard

- Erlaubt: `scheduler/src/**`, `scheduler/tests/**`, `scheduler/Cargo.toml` (nur Dev-Deps).
- Verboten: `scheduler/migrations/**` (eingefroren), `.github/**`, `compose/**`, `deploy/**`,
  `env/**`, andere Crates (`portal/`, `tapes/`), Workspace-`Cargo.toml` (außer zwingend).
- Kein Push auf `v2`/`main`. Commit-Style: `feat(#10): <english>` / `test(#10)` / `style(#10)`.

## Offene Punkte (an Orchestrator/Reviewer)

- O-1: Auth/RBAC-Middleware ist **nicht** Teil von #10 — Handler tragen `TODO(auth)`.
- O-2: Ob `clear`/Item-Mutationen `queues.version` erhöhen sollen, ist in §4.2 nicht
  explizit; **Vorschlag**: ja (Konsistenz), Version-Pflicht nur bei Reorder + Queue-PATCH.
- O-3: Branch-Historie enthält `b69d541` (UUIDv7 #50), der nicht auf `v2` (ae72483) liegt —
  Setup-Stage soll die Basis klären (kein Planner-Scope).
