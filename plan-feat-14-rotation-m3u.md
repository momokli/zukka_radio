# Plan: Issue #14 — Rotation-M3U

Basis: `v2` (origin/v2 @ 25baf76) · Branch `feat/14-rotation-m3u` · PR-Ziel `v2` · `Closes #14`
Recon-Quelle: `progress-feat-14-rotation-m3u.md`

## 1. Ziel & Deliverable

- **Ziel:** `GET /playback/rotation.m3u` liefert eine gültige M3U mit absoluten
  Music-API-URLs; der Scheduler hält die Rotation-Datei auf dem Volume
  `rotation` **atomar** aktuell, sodass Liquidsoaps
  `playlist("/rotation/rotation.m3u", reload_mode="watch")` sie **ohne
  Neustart / Control-Call** aufgreift (liquidsoap.md D8, Akzeptanz „Eine
  geänderte `rotation.m3u` wirkt ohne Neustart"; roadmap.md M1).
- **Deliverable des Gesamtruns:** gepushter Branch `feat/14-rotation-m3u` +
  offener PR gegen `v2` mit Body `Closes #14`. Stage 1 liefert nur diesen Plan.

## 2. Endpunkt-Matrix

| Methode | Pfad | Erfolg | Content-Type | Auth |
| ------- | ---- | ------ | ------------ | ---- |
| `GET` | `/api/v1/playback/rotation.m3u` | `200` (Body = M3U) | `audio/x-mpegurl` | Token-Layer (401 ohne/bei falschem Token) |

- Route liegt im `/playback`-Sub-Router (`playback::router()`), damit gilt der
  bestehende Token-Layer (`auth::require_token`, Header `x-liquidsoap-token`,
  fail-closed) **ohne Zusatzarbeit** — konsistent zu #13. Kein `204`-Fall: eine
  leere Rotation ist eine **gültige, leere** M3U (`200`).
- Fehlerfälle laufen über den bestehenden §4.6-Envelope (`ApiError`).

## 3. Entscheidungen der 5 offenen Design-Fragen

### a) Rotation-Inhalt — Projektion aus `tracks` mit `visibility='shared'`

- **Entscheidung:** Rotation = **alle** Zeilen aus `tracks` mit
  `visibility='shared'` **und** `music_api_id <> ''`, Reihenfolge `ORDER BY id`
  (deterministisch).
- **Begründung:** Docs definieren keine eigene Rotation-Tabelle und keinen
  `settings`-Key (data-model.md §3.1.3 kennt nur `tracks`, `visibility
  shared|private` mit `CHECK`, `music_api_id NOT NULL UNIQUE`; konsolidierte
  Architektur §3.4 `SourceKind = rotation` ist nur ein Layer-Name, keine
  Tabelle). `shared` ist die einzige dokumentierte, nicht personengebundene
  Sichtbarkeit (`private` = caster-gebunden, data-model.md offene Frage
  Zeile 1312) → kanonische Projektion der „gemeinsamen Library" als Rotation.
  Randomisierung ist **nicht** Aufgabe des Schedulers: sie gehört in Liquidsoap
  (`mode="randomize"`, liquidsoap.md §3.329), damit die Datei stabil bleibt und
  `watch` nur bei echter Inhaltsänderung neu lädt.

### b) Datei-Pfad / Env-Var

- **Entscheidung:** Default-Pfad `/rotation/rotation.m3u`; überschreibbar via
  **`ROTATION_M3U_PATH`**.
- **Begründung:** §5.5 „Pfade in den Containern": Volume `rotation` → `/rotation`,
  liquidsoap **ro**, scheduler **rw**; compose/{prod,dev}.yml mountet
  `rotation:/rotation` (scheduler rw). Das reale Script
  `liquidsoap/lib/layers.liq:5` liest bereits exakt
  `playlist("/rotation/rotation.m3u", reload_mode="watch")`. Die Angabe
  `/tracks/rotation.m3u` in liquidsoap.md D8 ist **veraltet** und wird durch
  C49 („Volume `rotation`, Scheduler schreibt atomar") + §5.5 aufgelöst.
  Env-Override ist nötig, damit der Integrationstest nicht auf `/rotation`
  schreiben muss. Default = Produktionspfad.

### c) Schreib-Trigger + atomares Schreiben

- **Entscheidung:**
  1. **Beim Start** (`main`): einmal atomar schreiben (best-effort) → Datei
     existiert ab Boot, `watch` sieht sofort eine valide Datei.
  2. **Bei jedem `GET /playback/rotation.m3u`**: Inhalt rendern, **atomar**
     (temp + `rename`) auf den Pfad schreiben (best-effort, Fehler nur loggen),
     und **immer** den gerenderten Inhalt als `200` zurückgeben.
- **Atomar:** `write_atomic(path, bytes)` schreibt in eine temporäre Datei
  **im selben Verzeichnis** (`<dir>/.rotation.m3u.<pid>.tmp`), `fsync`, dann
  `std::fs::rename` auf das Ziel (rename ist auf demselben Filesystem atomar).
  Kein Teilzustand sichtbar → erfüllt `reload_mode="watch"` (D8: „Scheduler
  schreibt die Datei atomar (temp + rename). Kein Neustart, kein Control-Call").
- **Begründung Trigger:** Es gibt (noch) keine `tracks`-CRUD im Scheduler, also
  keinen natürlichen Mutations-Hook; ein Datei-Watcher/Background-Timer wäre
  zusätzliche Komplexität ohne Doc-Forderung. „Boot-Write + Refresh-on-GET"
  garantiert Existenz und Aktualität ohne neue Infrastruktur. Write-Fehler
  dürfen den Endpunkt **nicht** zu `500` machen (der HTTP-Konsument bekommt den
  Inhalt trotzdem; Tests laufen ohne schreibbares `/rotation`).
- **Nicht in Scope:** automatischer Watcher/Interval-Task (Folge-Issue möglich).

### d) Auth

- **Entscheidung:** Der bestehende Token-Layer auf `/playback/*` greift; kein
  neuer Auth-Code. Fehlender/falscher Token → `401 unauthorized`.
- **Begründung:** §4 API-Tabelle führt `/playback/*` unter „Playback (nur
  Liquidsoap-Token)"; konsistent zu #13 (`auth.rs`).

### e) M3U-Format (exakt)

- Zeilenmodell (LF-getrennt, Datei endet mit LF):
  ```
  #EXTM3U
  #EXTINF:<sec>[,<artist> - <title>]
  <base>/stream/tracks/<music_api_id>
  ```
  je Track drei Zeilen in `ORDER BY id`-Reihenfolge.
- `<sec>` = `duration_ms / 1000` (ganzzahlige Division, abgerundet).
- Titelzeile: `{artist} - {title}` wenn `artist` nicht leer, sonst `{title}`
  (kein führender `" - "`). **Escaping:** `\r`/`\n` (und `\r\n`) in `artist`/
  `title` werden zu je einem Leerzeichen ersetzt, damit die zeilenbasierte M3U
  nicht zerbricht; übrige Zeichen (inkl. `,`, `#`) bleiben unverändert (nur das
  erste Komma nach der Dauer trennt Dauer/Titel).
- **Absolute** URL `<base>/stream/tracks/<music_api_id>` mit
  `base = music_api_base()` (env `MUSIC_API_BASE_URL`, Default
  `http://127.0.0.1:5001`) — identisches URIs-Muster/C46 wie
  `store::playback` (`{base}/stream/tracks/{music_api_id}`).
- Leere Rotation → Body exakt `#EXTM3U\n` (gültig, spielt nichts).
- Antwort-Header: `Content-Type: audio/x-mpegurl`. Kein JSON-Wrapping.

## 4. Konkrete Datei-Änderungen

**Kein Schema-Change.** `scheduler/migrations/0001_init.sql` bleibt eingefroren
(append-only) — #14 braucht keine Migration.

1. **`scheduler/src/store/rotation.rs` (NEU)**
   - `fn path() -> PathBuf` — liest `ROTATION_M3U_PATH`, Default
     `/rotation/rotation.m3u`.
   - `async fn render(pool: &SqlitePool) -> ApiResult<String>` — Query
     `SELECT music_api_id, title, artist, duration_ms FROM tracks WHERE
     visibility='shared' AND music_api_id <> '' ORDER BY id`; baut den
     M3U-String (§3e) mit `music_api_base()`.
   - `fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()>` —
     temp + fsync + rename; legt Elternverzeichnis via `create_dir_all` an.
   - `async fn sync(pool: &SqlitePool) -> ApiResult<()>` — `render` + atomar
     nach `path()` schreiben (Fehler propagieren, Aufrufer loggt).
   - Für Wiederverwendung: `music_api_base()` aus `store::playback` als
     `pub(crate)` freigeben **oder** in `rotation.rs` duplizieren (kleinster
     Eingriff: `pub(crate) fn music_api_base()` in `playback.rs` → `use` in
     `rotation.rs`).
2. **`scheduler/src/store/mod.rs`** — `pub mod rotation;` ergänzen.
3. **`scheduler/src/api/playback.rs`**
   - Route registrieren: `.route("/rotation.m3u", get(rotation_m3u))` im
     `router()` (bleibt hinter dem Token-Layer).
   - Handler `async fn rotation_m3u(State(pool): State<SqlitePool>) ->
     ApiResult<Response>`: `let body = store::rotation::render(&pool).await?;`
     → best-effort `store::rotation::sync(&pool).await` (Fehler per
     `tracing::warn!`), dann
     `Ok(Response::builder().header(CONTENT_TYPE, "audio/x-mpegurl")
     .body(Body::from(body))...)`.
4. **`scheduler/src/api/mod.rs`** — **keine** Änderung (Route lebt im
   gemounteten Sub-Router).
5. **`scheduler/src/main.rs`** — nach DB-Connect einmal
   `let _ = store::rotation::sync(&pool).await` mit `tracing::warn!` bei Fehler
   (Boot-Garantie, §3c).
6. **`scheduler/src/dto.rs`** — **keine** Änderung (Antwort ist roher Text).
7. **Tests (neu):** `scheduler/tests/rotation_m3u.rs` (§5); Unit-Tests inline
   in `store/rotation.rs` (`#[cfg(test)]`).

## 5. Teststrategie

Integrationstests unter `scheduler/tests/rotation_m3u.rs`, Harness wie
`playback_api.rs`: `zukka_scheduler::api::router(pool)` + `tower::oneshot`,
`tempfile::TempDir`, Env via `std::sync::Once`/`set_var`. `Once` setzt:
`LIQUIDSOAP_TOKEN="test-liquidsoap-token"`,
`MUSIC_API_BASE_URL="http://music.test"`,
`ROTATION_M3U_PATH=<std::env::temp_dir()>/zukka-rot-test-<pid>/rotation.m3u`.
Seeds via direkter SQL (`tracks` direkt, kein API).

| # | Test | Erwartet |
| - | ---- | -------- |
| 1 | `valid_m3u_projection` | shared+private seeden → `200`, Header `audio/x-mpegurl`, Body beginnt `#EXTM3U`, enthält **nur** shared-URLs `http://music.test/stream/tracks/<id>` (absolut), `#EXTINF:<sec>,<artist> - <title>` korrekt, private-Track fehlt |
| 2 | `empty_rotation` | keine Tracks → `200`, Body == `#EXTM3U\n` |
| 3 | `token_missing_401` | ohne Header → `401`, `error.code == "unauthorized"`; falscher Token → `401` |
| 4 | `escaping_newlines` | Track mit `\n`/`\r` in `artist`/`title` → Body enthält **keine** rohe Zeilenschaltung innerhalb `#EXTINF`, keine Extra-Zeile |
| 5 | `writes_file_atomically` | nach `GET` existiert `ROTATION_M3U_PATH`, Inhalt beginnt `#EXTM3U` (race-sicher: nur Präfix prüfen), kein `.tmp`-Rest im Verzeichnis |
| 6 | `order_deterministic` | zwei Tracks → Reihenfolge `ORDER BY id` (Body-Reihenfolge prüfen) |

Unit-Tests in `store/rotation.rs`:
- `write_atomic_creates_and_replaces` — tempdir, Ziel schreiben, dann überschreiben:
  Inhalt == letzter, keine temp-Restdatei.
- `render_skips_empty_music_api_id` (leerer `music_api_id` wird ausgelassen). 

## 6. Definition of Done

- [ ] `GET /api/v1/playback/rotation.m3u` liefert `200` + `audio/x-mpegurl`
      mit gültiger M3U inkl. absoluter Music-API-URLs; ohne Token `401`.
- [ ] Scheduler schreibt `/rotation/rotation.m3u` atomar (temp + rename) beim
      Boot und bei jedem `GET` (best-effort); `ROTATION_M3U_PATH` überschreibt.
- [ ] Keine Schema-/DTO-Änderung; `0001_init.sql` unverändert.
- [ ] Neue Tests (§5) grün.
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Branch `feat/14-rotation-m3u` gepusht, PR gegen `v2` offen mit
      `Closes #14`.

## 7. Reihenfolge

1. `store/rotation.rs` (+ `mod.rs`) — `render`, `write_atomic`, `path`, `sync`.
2. `store/playback.rs` — `music_api_base` → `pub(crate)` (oder duplizieren).
3. `api/playback.rs` — Route + Handler.
4. `main.rs` — Boot-Write.
5. Tests (§5), dann DoD-Kommandos (§6).
