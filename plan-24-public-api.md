# Plan: Public-Tapes-API (Issue #24, `tapes`-Crate)

Read-only Public-API auf **einer** DB `zukka.db` (`docs/consolidated-architecture.md` §4.5/§4.6,
`docs/tapes.md` §3.4). Die `tapes`-Crate ist heute nur ein `/healthz`-Skeleton
(`tapes/src/main.rs`) und hat **keine** Migrationen. Sie öffnet `zukka.db` ausschließlich
`mode=ro` (P2, tapes.md D4) und liest Schema/Tabellen des Schedulers
(`scheduler/migrations/0001_init.sql`: `tapes`, `casters`, `shows`).

## Verbindliche Design-Entscheidungen (für alle Stories)

- **Kein `sqlx::migrate!` in `tapes`.** Pool via `SqliteConnectOptions::new().filename(p).read_only(true)`.
  `journal_mode`/`synchronous` dürfen bei RO **nicht** gesetzt werden (SQLite würde
  `SQLITE_READONLY` werfen) — nur `busy_timeout` + `foreign_keys(true)` sind zulässig. `max_connections` klein (2–4).
- **Keine Compile-Time-SQL-Makros** (`sqlx::query!`): nutze `sqlx::query_as::<_, Row>` (Runtime-API),
  damit der Build keine `DATABASE_URL`/`.sqlx`-Offline-Daten braucht.
- **`public_tape`-Guard** als *eine* zentrale Funktion: `visible = 1 AND status = 'ready'`.
  Alle Nicht-ready-/unsichtbaren/`deleted`/unbekannten Slugs → **identisches** `404`
  `{"error":{"code":"not_found","message":"tape <slug> not found"}}` (kein Existenz-Leak, R9).
- **`caster` ist ein Join auf `casters`** (C01/C04/C08/C10), keine eigene Projektion
  (Abweichung zu tapes.md D17). `casters` hat **kein** `accent`/`sort_order`/`hidden`
  → `accent: null` (bzw. weglassen) und Casters-Sortierung nach `display_name`.
  `caster.id` = `casters.id` (UUIDv7); `caster.name` = `casters.display_name`.
- **Zeit** über `chrono-tz` (`Europe::Berlin`), Muster aus `scheduler/src/time.rs`
  (nach `tapes/src/` portieren, nicht importieren). Storage = RFC3339 UTC ms; API-Ausgabe
  Berlin mit Offset; `from`/`to` = **Berliner Kalendertage** → `[00:00 Berlin from, 00:00 Berlin to+1)`.
- **Cursor** = `base64url(json!({"t": <started_at_ms>, "s": "<slug>"}))`, Keyset auf
  `(started_at DESC, slug DESC)` (newest) bzw. aufsteigend (oldest).
- **ETag** = Hash aus `updated_at` + `slug` (`sha2`, workspace-dep vorhanden).

## Betroffene Dateien

Neu (alle unter `tapes/src/`):
- `config.rs` — Env: `DATABASE_PATH` (default `/data/zukka.db`), `ZUKKA_BIND`, `PUBLIC_BASE_URL`.
- `db.rs` — read-only Pool (kein `migrate!`).
- `error.rs` — §4.6-Envelope (`not_found`, `bad_request`, `internal`), `IntoResponse`.
- `model.rs` — `TapeDto`, `TapeListItemDto`, `CasterRefDto`, `ShowRefDto`, `CasterSummaryDto`, Fehlerobjekt.
- `time.rs` — `parse_storage_time`, `fmt_utc_z`, `fmt_api_time` (Offset), `window_label`, `berlin_day_start_utc` (chrono-tz).
- `cursor.rs` — `encode(Cursor)` / `decode(&str) -> Result<Cursor, ApiError>` (base64url + serde_json).
- `etag.rs` — `etag_for(updated_at, slug) -> String` (sha2), `if_none_match`-Check.
- `store/tapes.rs` — `public_tape_by_slug`, `feed` (Keyset + Filter), `row_to_dto`.
- `store/casters.rs` — `list_public` (Aggregat `tape_count`/`last_tape_at`).
- `api/mod.rs` — Public-Router `/api/v1`, State = `AppState { pool, base_url }`, `not_found`-Fallback.
- `api/tapes.rs` — Handler `get_tapes` (Feed), `get_tape` (Detail).
- `api/casters.rs` — Handler `get_casters`.
- `main.rs` — Wiring: config → db → router → serve; `/healthz` + `/api/v1/healthz`.

Geändert:
- `tapes/Cargo.toml` — `base64.workspace = true`, `sha2.workspace = true` ergänzen
  (chrono-tz/serde_json/sqlx/tower-http sind via workspace vorhanden).

Tests: `tapes/tests/public_api.rs` + Fixture-Helper (`tapes/src/fixtures.rs` oder `#[cfg(test)]`),
In-Memory-/tmp-SQLite, das das Scheduler-Schema lädt (`0001_init.sql` per `include_str!`), kein Netz.

---

## User Stories (in Reihenfolge)

### US-1 — Fundament: read-only Pool, Config, Error-Envelope, Healthz
- **Akzeptanzkriterien**
  - `tapes` startet mit `DATABASE_PATH` auf eine existierende `zukka.db`-Kopie und **führt keine
    Migration/Auto-DDL** aus (kein `sqlx::migrate!`; DB-File unverändert nach Start — `PRAGMA data_version` stabil).
  - Write-Versuch über den Pool schlägt fehl (`SQLITE_READONLY`), d. h. Pool ist wirklich read-only.
  - `GET /api/v1/healthz` → `200`, führt einen trivialen `SELECT` gegen `tapes` aus; DB-Fehler → `503`
    mit §4.6-Envelope, `Cache-Control: no-store`. `GET /healthz` bleibt `200 "ok"`.
  - Unbekannte Route → `404` mit `{"error":{"code":"not_found",...}}` (kein Axum-Plaintext).
  - `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` grün.
- **Betroffene Dateien**: `config.rs`, `db.rs`, `error.rs`, `api/mod.rs`, `main.rs`, `Cargo.toml`.
- **Geschätzt**: ~220 LOC.

### US-2 — Einzel-Tape `GET /api/v1/tapes/{slug}` + `public_tape`-Guard
- **Akzeptanzkriterien**
  - JSON per §3.3 (tapes.md): `slug`, `caster{id,name,accent:null}`, `show{id,name}|null`,
    `title` (roh, `null` möglich), `display_title`, `description`, `started_at`/`ended_at` (UTC `…Z`),
    `start_local`/`end_local` (expliziter Offset `+01:00`/`+02:00`), `tz:"Europe/Berlin"`,
    `window_label:"DD.MM.YYYY HH:MM – HH:MM"`, `duration_ms`, `source`, `format`, `mime`,
    `size_bytes`, `audio_url`, `download_url`, `waveform_url`, `peaks` (oder `null`), `share_url`,
    `published_at`, `updated_at`. **Nie** öffentlich: `status`, `visible`, `storage_key`, `etag`.
  - `display_title`-Fallback: `title` (nicht leer) > `shows.name` > `casters.display_name`.
  - Guard: `visible=0` **oder** `status != 'ready'` (inkl. `pending`/`processing`/`failed`/`deleted`)
    → **exakt** dieselbe `404`-Antwort wie ein nie existierender Slug (byte-gleicher Body).
  - `peaks` = `null`, wenn keine `waveforms`-Zeile `(subject_kind='tape', subject_id=<tapes.id>)` existiert.
  - `*_url`-Felder absolut aus `PUBLIC_BASE_URL`.
  - Akzeptanz #3/#4 (tapes.md §7): DST-Fall `2026-10-25T00:30:00Z` → `01:30+02:00`,
    `2026-10-25T01:30:00Z` → `02:30+01:00`.
- **Betroffene Dateien**: `model.rs`, `time.rs`, `store/tapes.rs`, `api/tapes.rs`, `api/mod.rs`, `main.rs`.
- **Geschätzt**: ~260 LOC.

### US-3 — Feed `GET /api/v1/tapes` mit Filtern, Keyset-Cursor, `next_cursor`/`count`
- **Akzeptanzkriterien**
  - Query-Parameter: `caster` (wiederholbar **und** CSV → Menge von `casters.id`),
    `from`/`to` (`YYYY-MM-DD`, Berliner Kalendertage → UTC-Grenzen via chrono-tz; `to` exklusiv = `to+1` 00:00 Berlin),
    `sort` (`newest` default | `oldest`), `limit` `1..=200` (default 50), `cursor` (opaque).
  - Trefferbasis immer `visible=1 AND status='ready'`; Keyset `(started_at DESC, slug DESC)` (bzw. ASC),
    stabile Reihenfolge ohne Offset.
  - Antwort: `{ "tapes": [...], "next_cursor": <string|null>, "count": <n>, "server_time": "<utc Z>" }`.
    `next_cursor` ist gesetzt **nur** wenn noch Treffer vorhanden sind; der Folgecall mit diesem
    Cursor liefert **keine** Überschneidung mit der ersten Seite (Akzeptanz #1).
  - `cursor`-Dekodierung: ungültiges Base64/JSON/fehlende Felder → `400 {"error":{"code":"bad_request",...}}`.
  - `limit` außerhalb `1..=200` → `400`; `from > to` → `400`.
  - `server_time` = aktueller UTC-Instant (`…Z`).
  - Hinweis (nicht-blockierend): der Keyset-Scan nutzt idealerweise einen Index
    `tapes(visible,status,started_at,slug)` (tapes.md `tape_feed`); das Schema hat ihn **nicht**
    (nur `idx_tapes_status_visible`). Da `tapes` keine Migrationen besitzt, ist ein Index nur über
    eine Scheduler-Migration möglich → als Folge-Issue notieren, nicht in diesem Scope.
- **Betroffene Dateien**: `cursor.rs`, `store/tapes.rs`, `api/tapes.rs`, `model.rs`, `time.rs`.
- **Geschätzt**: ~300 LOC.

### US-4 — Caster-Aggregat `GET /api/v1/casters`
- **Akzeptanzkriterien**
  - Antwort = Array von `{ id, name, accent: null, tape_count, last_tape_at }` (tapes.md §3.4).
  - Nur Caster mit `tape_count > 0` (nur `visible=1 AND status='ready'` zählen); `tape_count` =
    `COUNT(*)`, `last_tape_at` = `MAX(started_at)` (UTC `…Z`).
  - Sortierung: `display_name` aufsteigend (kein `sort_order` im Schema).
  - Caster, deren einzige Tapes unsichtbar sind, erscheinen **nicht** (Akzeptanz #17, Zähler-Leak R9).
- **Betroffene Dateien**: `store/casters.rs`, `api/casters.rs`, `model.rs`, `api/mod.rs`.
- **Geschätzt**: ~120 LOC.

### US-5 — Caching/ETag/304, CORS, 405 & Header
- **Akzeptanzkriterien**
  - ETag für Detail (`/tapes/{slug}`) und Feed; `Cache-Control: max-age=300` (Detail) bzw. `max-age=60` (Feed),
    `public`. `If-None-Match: <ETag>` → `304` ohne Body, mit erhaltenem `ETag`/`Cache-Control` (Akzeptanz #5).
  - ETag ändert sich, wenn `tapes.updated_at` sich ändert; gleicher ETag bei gleichem Inhalt.
  - `OPTIONS` auf Public-Routen → `204` mit `Access-Control-Allow-Origin: *`,
    `Access-Control-Allow-Methods: GET, HEAD, OPTIONS`, `Access-Control-Max-Age: 86400`
    (kein `Allow-Credentials`); jede `200`/`304`-Antwort trägt `Access-Control-Allow-Origin: *`.
  - `POST`/`PUT`/`PATCH`/`DELETE` auf Public-Routen → `405` mit `Allow: GET, HEAD, OPTIONS`.
  - `healthz` bleibt `no-store`.
- **Betroffene Dateien**: `etag.rs`, `api/mod.rs`, `api/tapes.rs`, `api/casters.rs`, `main.rs`.
- **Geschätzt**: ~160 LOC.

### US-6 — Fixtures, Integrationstests & Verifikation
- **Akzeptanzkriterien**
  - Test-Fixture lädt das Scheduler-Schema (`0001_init.sql`) in eine temporäre SQLite-Datei und
    seedet Rollen/`casters`/`shows`/`tapes` (ready/sichtbar, ready/unsichtbar, `pending`, `deleted`,
    ohne `waveform`, DST-Grenzfälle 2026-10-25).
  - Tests: (a) Guard/Body-Gleichheit unsichtbar vs. unbekannt; (b) `display_title`-Kette (3 Stufen);
    (c) Feed-Pagination ohne Überschneidung + `next_cursor==null` am Ende; (d) Caster-Filter single/CSV/Repeat;
    (e) `from`/`to`-Berlin-Grenzen; (f) `404`/`400`/`405`-Envelope; (g) `304` bei passendem `If-None-Match`;
    (h) Casters-Aggregat ohne unsichtbare Tapes. DB-Read-Only bleibt nach Testlauf unverändert.
  - Kein Netzwerkzugriff in Tests.
- **Betroffene Dateien**: `tapes/tests/public_api.rs`, `tapes/src/fixtures.rs`.
- **Geschätzt**: ~280 LOC.

---

## Reihenfolge & Abhängigkeiten

`US-1 → US-2 → {US-3, US-4}` ; `US-3/US-4 → US-5 → US-6`.
- US-2 liefert `model.rs`/`time.rs`/Guard, auf denen US-3 und US-4 aufbauen.
- US-5 setzt auf den in US-2/US-3 gebauten Handlern auf (ETag-Wrapper + CORS/405-Layer).
- US-6 verifiziert alle vorherigen Stories (End-to-End).

## Offene Punkte / Risiken (melden, nicht lösen)

- **`casters`-Schema-Abweichung**: kein `accent`/`sort_order`/`hidden` in `0001_init.sql`
  (tapes.md D17 erwartet sie) → `accent` = `null`; Casters-Sortierung nach `display_name`.
- **Fehlender Feed-Index**: Keyset-Query ohne `tapes(visible,status,started_at,slug)`-Index;
  Index nur via Scheduler-Migration möglich (tapes read-only) → Folge-Issue im Scheduler-Repo.
- **`caster.id`-Semantik**: JSON nutzt `casters.id` (UUIDv7); tapes.md-Beispiel zeigt Username-Form.
  Portalkompatibilität prüfen (kann `caster.display_name` als Name abbilden).
