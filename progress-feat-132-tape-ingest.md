# Progress — feat/132-tape-ingest

Issue: [#132](https://github.com/momokli/zukka_radio/issues/132) — *tapes: Recording-Ingest fehlt (Staging -> Music API -> tapes-Row -> Waveform -> Feed)*
Branch: `feat/132-tape-ingest` · Base/PR-Ziel: `v2` · Milestone M10
Status: Stage 1 (Plan). Kein Produktionscode.

## Verifizierter Ist-Stand (gegen origin/v2 @ 316b216)

- `liquidsoap/lib/outputs.liq`: `output.file(%mp3(192), tape_path(), safe_radio, start=false, on_close=tape_on_close)`;
  `on_close` loggt nur. Datei liegt als `/recordings/<tape_id>.mp3`.
- `scheduler/src/store/tapes.rs`: `register_live_tape` (status=pending) und
  `finalize_live_tape` (setzt nur `ended_at`). Upload/Probe/Peaks sind laut Kommentar
  "out of scope for #108".
- `scheduler/src/store/broadcasts.rs::transition`: nach Commit wird `tape.stop` gepusht.
  Kein Ingest-Hook.
- `scheduler/src/store/tape_jobs.rs`: Queue fuer `probe`/`peaks` (CHECK kind IN ('probe','peaks')),
  Claim/Retry-Backoff existiert bereits, Worker ist read-only.
- `scheduler/migrations/`: 0001_init, 0002_show_announcement_timings. Naechste freie Nummer: 0003.
- `tapes`-Schema hat `storage_key`, `music_api_id`, `format`, `mime`, `size_bytes`, `etag`, `status`.
- Kein Music-API-Client im Scheduler; `reqwest` + `sha2` sind bereits Dependencies.
- Music-API-Contract (docs/portal.md, #134): `PUT /objects/{sha256}` (streaming, idempotent,
  Header `X-ISRC`, `X-Name`, `X-Original-Path`), `HEAD /objects/{sha256}` (200 vorhanden / 404 neu),
  `POST /work {"kind":"analyze"|"download", ref}`. Kein `/api/v1`-Praefix.
- `tapes/src/config.rs` liest `{MUSIC_API_URL}/objects/{key}`; `music_api_id` wird als
  Objekt-Hash verwendet (`playback.rs`: `playback_uri(base, None, music_api_id)`).

## Zielbild (docs/consolidated-architecture.md §3.6)

Broadcast endet -> scheduler `tape.stop` -> Upload MP3 nach Music API ->
`tapes`-Row aktualisieren (music_api_id=sha256, storage_key, size_bytes, etag,
format='mp3', mime, status='processing') -> `tape_jobs` (probe, peaks) einreihen ->
Staging-Datei erst nach erfolgreichem Upload loeschen.

Fehlerpfad: Upload-Fehler muessen retrybar sein (Job-Status), Staging-Datei bleibt liegen.

## Offene Design-Entscheidung fuer Stage 1

Upload-Job-Persistenz: `tape_jobs.kind` erweitern (Migration 0003, CHECK um 'upload')
ist naheliegend, damit der bestehende Claim/Retry-Mechanismus genutzt wird. Alternativ
eigene `tape_ingest_jobs`-Tabelle. Planner entscheidet und begruendet.

## Pipeline-Log

- 2026-11-21 Stage 0 (Orchestrator): Repo geklont, Branch von origin/v2, Ist-Stand erhoben.

## Plan

### Kernentscheidungen

**E1 — Upload-Job-Persistenz: bestehende `tape_jobs`-Mechanik nutzen (Migration 0003).**
`tape_jobs.kind` bekommt `'upload'` (CHECK → `IN ('probe','peaks','upload')`). Begruendung:
Claim mit Lease-Recovery, `attempts`-Zaehler und Backoff (`backoff_secs`) existieren bereits und
sind getestet; `UNIQUE (tape_id, kind)` liefert die Idempotenz (hoechstens ein Upload-Job pro
Tape) gratis; der Scheduler bleibt Single-Writer einer einzigen Queue. Eine eigene Tabelle
muesste Claim/Lease/Backoff duplizieren — kein Mehrwert.
*Guard:* Der worker-zugewandte Endpoint `/internal/tape-jobs/claim` darf `upload` **nicht**
claimbar machen (der read-only tapes-worker verarbeitet nur probe/peaks). `KINDS` bleibt
`['probe','peaks']`; der Ingest-Task ruft `store::tape_jobs::claim(pool, &["upload"], …)` direkt
auf. Der PATCH-Route wird `kind='upload'` explizit verboten (409/400).

**E2 — Ingest wird scheduler-intern getrieben.** Neues Modul `scheduler/src/tape_ingest.rs` mit
`run_once(pool, &IngestConfig) -> Report` und `spawn(pool, config) -> JoinHandle<()>`, gestartet in
`main.rs` neben `announcement::dispatch::spawn` (Analyogie-Pattern, §2.1/§3.6: „Scheduler laedt
hoch"). Poll-Schleife claimt faellige `upload`-Jobs (limit ≥ 1), laedt pro Job hoch und schreibt
den Ausgang in die DB. Kein HTTP-Write-Through, keine Beteiligung des tapes-workers.

**E3 — Idempotenz/Retry-Protokoll pro Job:**
1. `sha256` **streaming** ueber die Staging-Datei berechnen (Pass 1, `tokio::fs` + `sha2`, nie im
   RAM; Hex manuell via `{:02x}`, kein `hex`-Dependency).
2. `HEAD /objects/{sha256}` (Bearer `MUSIC_API_TOKEN`). `200` → Objekt existiert, `PUT`
   ueberspringen (Dedupe); `404` → `PUT /objects/{sha256}` mit Body als Datei-Stream (Pass 2,
   `reqwest` `Body::wrap_stream`), Headern `X-Name` (slug/title), `X-Original-Path`
   (`/recordings/<tape_id>.mp3`). Kein `X-ISRC` (Tape ohne ISRC).
3. Erfolg → `complete_upload(...)`: in **einer** Tx `tapes` fuellen
   (`music_api_id=sha256, storage_key=music_api_id, size_bytes, etag, format='mp3',
   mime='audio/mpeg', status='processing'`) **und** `tape_jobs` (kind `probe`, `peaks`,
   `state='pending'`) per `INSERT` einreihen **und** den `upload`-Job auf `done` setzen.
4. **Nur danach** die Staging-Datei loeschen (idempotent, `NotFound` ignorieren).
5. Fehler (Netz/5xx/Hash-Fehler) → `fail_upload(...)`: Job `failed` mit `error`, `tapes.status`
   bleibt `pending` (nicht `failed`), **Staging-Datei bleibt liegen**. Der bestehende
   `claim`-Backoff (`attempts`, `updated_at`) requeued den Job automatisch; bei Lease-Timeout wird
   ein haengender `running`-Job wieder `pending`. Erst ein terminales `attempts`-Cap (z. B. 8)
   setzt `tapes.status='failed'` — Datei bleibt fuer manuellen Recovery erhalten.

**E4 — Teststrategie: hermetic, ohne neue Mock-Abhaengigkeit.** Bestehendes Muster
(`tape_recording.rs`, `telegram_delivery.rs`): minimaler HTTP/1.1-Server auf
`TcpListener::bind("127.0.0.1:0")`, der Requests liest und konfigurierbare Antworten (Status/Body)
liefert — kein WireMock nötig. `IngestConfig { music_api_base, token, staging_root }` ist
injizierbar, damit Tests keinen Prozess-Env-Zwang haben (Dateipfad-Root + base_url als Parameter,
nicht nur `env::var`).

### Betroffene Dateien

- `scheduler/migrations/0003_tape_ingest.sql` (neu)
- `scheduler/src/tape_ingest.rs` (neu)
- `scheduler/src/lib.rs`, `scheduler/src/main.rs`
- `scheduler/src/store/tape_jobs.rs`
- `scheduler/src/store/tapes.rs`
- `scheduler/src/api/tape_jobs.rs`
- `scheduler/src/dto.rs` (nur falls internes DTO noetig)
- `scheduler/tests/tape_ingest.rs` (neu), `scheduler/tests/tape_jobs_api.rs` (kleiner Zusatz)
- `docs/consolidated-architecture.md` §3.6, `docs/tapes.md` (Ist-Stand-Notiz)

### Migration 0003 (Detail)

SQLite kann ein `CHECK` nicht per `ALTER` aendern → Tabellen-Rebuild von `tape_jobs`:

```sql
-- 0003_tape_ingest.sql
CREATE TABLE tape_jobs_new (
    id TEXT PRIMARY KEY, tape_id TEXT NOT NULL REFERENCES tapes(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('probe','peaks','upload')),
    state TEXT NOT NULL DEFAULT 'pending'
          CHECK (state IN ('pending','running','done','failed')),
    error TEXT, attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    UNIQUE (tape_id, kind)) STRICT;
INSERT INTO tape_jobs_new SELECT id,tape_id,kind,state,error,attempts,created_at,updated_at
    FROM tape_jobs;
DROP TABLE tape_jobs;
ALTER TABLE tape_jobs_new RENAME TO tape_jobs;
```

Kein `PRAGMA foreign_keys`-Toggle nötig: **keine** andere Tabelle referenziert `tape_jobs`, alle
FKs zeigen outward (auf `tapes`), die kopierten Zeilen sind gueltig. Der Inline-`UNIQUE` wird beim
Rename mitgenommen. `sqlx::migrate!` wendet 0003 wie 0002 in einer Transaktion an.

### User Stories (Reihenfolge = Abhaengigkeit)

1. **US-1 Migration + Kind-Konstante**
   - AC: `tape_jobs.kind='upload'` insertierbar; `probe/peaks` unveraendert; Bestandszeilen
     erhalten; `store::tape_jobs::KINDS` bleibt `['probe','peaks']`, neue Konstante
     `UPLOAD_KIND="upload"`.
   - Dateien: `migrations/0003…`, `store/tape_jobs.rs`. ~25 LoC.

2. **US-2 Upload-Outcome-Store-Funktionen**
   - AC: `complete_upload(pool, job_id, tape_id, outcome)` setzt in einer Tx `tapes`-Felder auf
     `processing`, reiht `probe`+`peaks` ein (`ON CONFLICT DO NOTHING`), Job `done`.
     `fail_upload(pool, job_id, tape_id, error)` setzt Job `failed`, laesst `tapes.status`
     unberuehrt (retrybar). Beide pruefen die Transition (409 bei `done`→`failed`).
   - Dateien: `store/tape_jobs.rs`, ggf. `store/tapes.rs`-Helper. ~90 LoC.

3. **US-3 Ingest-Task (Hash + HEAD/PUT + Datei-Lifecycle)**
   - AC: `run_once` claimt `upload`-Jobs, hasht streaming, HEAD-dedupe, PUT-Stream mit Headern,
     ruft US-2, loescht Staging-Datei **nur** nach `done`; Fehler → `fail_upload`, Datei bleibt.
     `spawn` startet die Poll-Schleife; ohne `MUSIC_API_TOKEN`/Base-URL sauberer No-Op+Log.
   - Dateien: `tape_ingest.rs`, `lib.rs`, `main.rs`. ~240 LoC.

4. **US-4 Worker-Route-Guard**
   - AC: `/internal/tape-jobs/claim` lehnt `kind`-Wert `upload` mit `400 validation` ab; default
     bleibt `probe/peaks`; `PATCH` auf einen `upload`-Job → `409`.
   - Dateien: `api/tape_jobs.rs`. ~12 LoC.

5. **US-5 Hermetischer Ingest-Test**
   - AC (siehe Teststrategie): Happy Path, Dedupe-HEAD-200, PUT-5xx-Retry, Connection-Refused,
     Datei-Lifecycle, `tapes`-Felder, Job-Enqueue — alles gegen den lokalen Mock-Server.
   - Dateien: `tests/tape_ingest.rs`, kleiner Zusatz in `tests/tape_jobs_api.rs`. ~330 LoC.

6. **US-6 Doku**
   - AC: §3.6 nennt `upload`-Job, Backoff und „Datei erst nach done löschen"; tapes.md-Q6/Ist-Stand
     aktualisiert.
   - Dateien: Docs. ~15 LoC.

### Teststrategie (Detail)

Neuer Test-Binary `scheduler/tests/tape_ingest.rs`, Muster wie `telegram_delivery.rs`:

- **Mock-Music-API**: `TcpListener` auf `127.0.0.1:0`, Queue von vorprogrammierten Antworten
  `(Status, Body, gelesene Request-Zeile)`; zaehlt eingehende Requests und sammelt Method/Pfad/
  Header/Body fuer Assertions.
- **Fixture**: `TempDir` mit `zukka.db` (`db::connect`) und einem separaten
  `recordings/`-Verzeichnis; eine Staging-Datei mit bekanntem Inhalt (z. B. `b"hello zukka"`),
  deren `sha256` im Test vorberechnet wird. Seed: caster + tape (`source='live', status='pending'`)
  + ein `tape_jobs`-Row `kind='upload'`.
- **T1 Happy Path**: `run_once` → Mock sieht `HEAD /objects/{sha}` (404) dann `PUT` (200) mit
  korrektem Pfad/`X-Name`/Stream-Body; `tapes` hat `music_api_id==sha`, `storage_key`, `size_bytes`,
  `etag`, `format='mp3'`, `status='processing'`; `probe`+`peaks` pending; `upload`-Job `done`;
  Staging-Datei entfernt.
- **T2 Dedupe**: Mock `HEAD`→`200` → kein `PUT`; `tapes` trotzdem gefuellt; Datei entfernt.
- **T3 Retry**: Mock `PUT`→`500` (HEAD 404) → Job `failed`, `error` gesetzt, `tapes.status` bleibt
  `pending`, Datei existiert; danach `UPDATE tape_jobs SET updated_at='1970-…', attempts=0` und
  zweiter `run_once` gegen einen erfolgreichen Mock → `done`, Datei weg (belegt Requeue).
- **T4 Fehlerpfad ohne Server**: base_url auf unbenutzten Port → Job `failed`, Datei bleibt.
- **Unit-Tests** in `tape_ingest.rs`: Streaming-SHA256 gegen bekannten Vektor; Staging-Pfad-Helper
  (`{root}/{tape_id}.mp3`); Header-Aufbau.
- **Grenzen**: `schema_invariants.rs` bleibt unveraendert (prueft nur Tabellen/IDs, keine Kinds).
  `tape_jobs_api.rs` erhaelt einen Negativtest fuer den `upload`-Guard (US-4).

### Implementierungs-Reihenfolge

`US-1 → US-2 → US-3 → US-4 → US-5 → US-6` (Migration zuerst, dann Store, dann Task, dann Guard,
dann Tests/Docs). Jede Story einzeln `cargo test`-gruen.

### Offene Risiken

- **R-A (Rebuild)**: Migration 0003 ist ein Tabellen-Rebuild; bei grossem `tape_jobs`-Bestand
  kurzzeitig Lock. Akzeptabel (Queue ist klein); durch „keine inbound-FKs" risikoarm. Verifikation:
  `PRAGMA foreign_key_check` im Migrations-Smoke-Test.
- **R-B (Doppel-Read)**: Hash-Pass + Upload-Pass lesen die Datei zweimal (~170 MB × 2). Bewusst
  gewaehlt gegenueber RAM-Puffer; Alternative (spooled temp + tee) nicht noetig.
- **R-C (HEAD-Header)**: Ob `HEAD 200` `Content-Length`/`ETag` liefert, ist laut F4 noch nicht
  verifiziert. Fallback: `size_bytes` aus lokaler Datei, `etag` = sha256, wenn Header fehlen.
- **R-D (Staging-Root)**: Env-Name (`TAPES_STAGING_DIR`, Default `/recordings`) muss zur
  Compose-Mount-Realitaet (§5.5) passen — im Task konfigurierbar, damit Tests nicht auf
  Prozess-Env angewiesen sind.
- **R-E (Backoff-Testbarkeit)**: Requeue-Test manipuliert `updated_at`/`attempts` direkt in SQL;
  alternative Zeit-Injektion waere groesserer Umbau — bewusst nicht.
## Setup-Report (Stage 2)

Host: planet · Branch: `feat/132-tape-ingest` · Arbeitsbaum sauber.

### Baseline-Ergebnisse — GRÜN

| Check | Befehl | Ergebnis |
|---|---|---|
| Format | `cargo fmt --all -- --check` | exit 0 |
| Compile + Test-Binaries | `cargo test -p zukka-scheduler --no-run` | exit 0 (alle 36 Test-Binaries gebaut) |

Keine roten Checks. Keine Tool-/Zugriffs-Blocker.

### Migration 0003 — Kompatibilität mit sqlx-Migrate-Setup

- `sqlx::migrate!` ist eingebettet: `scheduler/src/db.rs:32` → `sqlx::migrate!("./migrations").run(&pool)`.
- Migrations-Verzeichnis: `scheduler/migrations/` mit `0001_init.sql`, `0002_show_announcement_timings.sql`.
  Nächste freie Nummer ist **0003** (bestätigt). `0002` nutzt `CREATE TABLE ... STRICT` ohne Transaktion;
  `sqlx::migrate!` wendet Migrationen je in einer Transaktion an → Rebuild-SQL (DROP/CREATE/RENAME) läuft atomar.
- Migrations-Smoke-Test existiert: `scheduler/tests/migration_smoke.rs`.
  `setup()` ruft `zukka_scheduler::db::connect()` (connect + migrate). Prüft exakt 18 User-Tabellen,
  leeren `PRAGMA foreign_key_check` und `PRAGMA integrity_check = ok`.
  → Rebuild von `tape_jobs` hält die Tabellenzahl bei 18; Smoke-Test bleibt grün.
- `scheduler/tests/schema_invariants.rs` prüft nur Tabellen/IDs, keine `kind`-Werte → vom Rebuild unberührt.
- **Inbound-FK-Check:** `grep REFERENCES tape_jobs` in `migrations/` + `src/` = 0 Treffer → keine Tabelle
  referenziert `tape_jobs`; Rebuild ohne `PRAGMA foreign_keys`-Toggle sicher (R-A bestätigt).
- Keine Migration ausgeführt (wie gefordert).

### Wiederverwendbarer HTTP-Mock-Test-Helper

- **Primär:** `scheduler/tests/telegram_delivery.rs`
  - `async fn mock_server(count: usize, status: u16, body: &'static str) -> Mock { base, handle }`
    — `TcpListener::bind("127.0.0.1:0")`, liefert `count` Antworten mit fixem Status/Body, sammelt Roh-Requests.
  - `async fn read_request(&mut TcpStream) -> String` — liest Request inkl. Content-Length-Body.
  - `fn request_body(&str) -> String` — trennt Header/Body.
  - Für US-5 generalisierbar zu einer Queue vorbestimmter Antworten (HEAD-404 → PUT-200 etc.).
- **Sekundär:** `scheduler/tests/tape_recording.rs` → `async fn mock_control_server(count) -> (String, JoinHandle<Vec<String>>)`.

### Setup-Fazit

Baseline grün. Migration 0003 (Rebuild `tape_jobs`) ist mit dem sqlx-Setup kompatibel; kein Blocker.
Developer kann den `mock_server`/`read_request`-Helper aus `telegram_delivery.rs` adaptieren.
Kein Produktionscode, keine Commits.

## Developer-Report (Stage 3)

Commit: `2881892` — `feat(#132): tape recording ingest (staging -> music api -> tapes -> jobs)`
Branch: `feat/132-tape-ingest` (gepusht nach origin, Basis `v2`).

### Geaenderte Dateien

| Datei | Aenderung |
|---|---|
| `scheduler/migrations/0003_tape_ingest.sql` | neu — `tape_jobs`-Rebuild, CHECK `('probe','peaks','upload')`, `idx_tape_jobs_state` neu angelegt |
| `scheduler/src/store/tape_jobs.rs` | `UPLOAD_KIND`, `MAX_UPLOAD_ATTEMPTS=8`, `UploadOutcome`, `complete_upload` (eine Tx: tapes→processing + probe/peaks enqueued ON CONFLICT DO NOTHING + upload done), `fail_upload` (Job failed, tapes unveraendert); PATCH auf upload-Job → 409 |
| `scheduler/src/tape_ingest.rs` | neu — `IngestConfig::from_env` (`MUSIC_API_BASE_URL`, `MUSIC_API_TOKEN`, `TAPES_STAGING_DIR`, Default `/recordings`), `run_once`, `spawn` (Poll-Loop, `TAPE_INGEST_POLL_SECS`, Default 10 s), Streaming-SHA256 (64 KiB), HEAD-Dedupe, PUT via `Body::wrap_stream` mit `X-Name`/`X-Original-Path`, Datei-Loeschung nur nach Commit, attempts-Cap → tapes failed |
| `scheduler/src/lib.rs`, `scheduler/src/main.rs` | Modul exportiert; Ingest-Spawn neben announcement dispatch, fail-open |
| `scheduler/src/api/tape_jobs.rs` | claim: `kind='upload'` → 400; Default-Kinds unveraendert |
| `scheduler/Cargo.toml` | `tokio-util` (io) fuer `ReaderStream` |
| `scheduler/tests/tape_ingest.rs` | neu — T1 Happy Path, T2 HEAD-200-Dedupe, T3 PUT-500 + Requeue, T4 unerreichbarer Server; Unit-Tests Streaming-Hash/Staging-Pfad/URL |
| `scheduler/tests/tape_jobs_api.rs` | Negativtest: upload nicht claimbar (400), PATCH → 409 |
| `scheduler/tests/schema_invariants.rs` | Migrationszaehler 2 → 3 (0003) |
| `docs/consolidated-architecture.md` | §3.6 Upload-Job, Fehlerpfad, „Datei erst nach Commit loeschen" |
| `docs/tapes.md` | Ist-Stand-Absatz #132 |

### Checks

- `cargo fmt --all -- --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test --workspace` → alle Test-Binaries gruen (0 failed); `tape_ingest` 4/4, `tape_jobs_api` 13/13, `schema_invariants` 21/21.

Hinweis: ein Lauf zeigte einen einmaligen Flake in `rotation_m3u::writes_file_atomically`
(paralleler Datei-Test); Einzel- und Wiederholungslauf gruen, kein Zusammenhang mit #132.

### Offen / bewusst nicht gemacht

- Kein automatisches Requeue von `failed`-Uploads: der Claim liest nur `pending`
  (bestehende Mechanik). Der Requeue-Test setzt explizit `state='pending'`;
  operatives Requeue bleibt manuell/SQL (wie im Plan dokumentiert).
- `music_api_id` ist `UNIQUE`: zwei byte-identische Tapes kollidieren beim
  `complete_upload` (Job failed, Datei bleibt) — Dedupe auf Objektebene, aber
  Tape-Row-Kollision nicht aufgeloest.
- Music-API-HEAD-/PUT-Header (`ETag`) sind laut Plan R-C unverifiziert; Fallback
  `etag = sha256`.
- Kein Deploy (Host planet).

## Verifier-Report (Stage 4, Quality Gate)

Verifier: `feature-dev-verifier` · Commit `2881892` · Basis `origin/v2...HEAD` · Host planet

### Teststatus — GRÜN

- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0, keine Warnungen
- `cargo test --workspace` → alle Binaries grün (0 failed); `tape_ingest` 4/4,
  `tape_jobs_api` 13/13, `schema_invariants` 21/21, `migration_smoke` 5/5
- Diff begrenzt auf `scheduler/**` + `docs/**` (keine Fremdänderungen)

### Ergebnis: **FAIL** (2 Blocker, 1 Minor)

#### 1. Migration 0003 — PASS
Rebuild `tape_jobs` (CHECK `('probe','peaks','upload')`), 1:1-Kopie aller Spalten, Index
`idx_tape_jobs_state` aus 0001 wird durch `DROP TABLE` mitentfernt und neu angelegt
(`0003…sql:32`). Kein inbound-FK (`grep REFERENCES tape_jobs` = 0). `sqlx::migrate!` wickelt
die Datei in einer Tx ab; `migration_smoke` (18 Tabellen, `foreign_key_check` leer,
`integrity_check=ok`) grün. Kein Datenverlust. Kosmetik: Datei ohne Schluss-Newline.

#### 2. Streaming — FAIL (Content-Length fehlt)
- Streaming-SHA256: **PASS** — `sha256_file` liest 64 KiB-Chunks, nie im RAM.
- HEAD-Dedupe: **PASS** — `HEAD 200` überspringt PUT (`tape_ingest.rs:216`).
- PUT-Stream/Header: **FAIL** — Header `X-Name`, `X-Original-Path`, `Content-Type: audio/mpeg`
  gesetzt, **aber kein `Content-Length`** (`tape_ingest.rs:236-245`). `Body::wrap_stream`
  erzwingt `Transfer-Encoding: chunked`. Vorgabe war „PUT-Stream mit Content-Length".
  `size_bytes` ist bereits bekannt (`tape_ingest.rs:200`) → Fix s. u.
- Dateilöschung nur nach `complete_upload Ok`: **PASS** (`tape_ingest.rs:264`).

#### 3. Retry-Semantik — FAIL (kein automatisches Requeue)
- `fail_upload` lässt `tapes.status` unberührt (`store/tape_jobs.rs:401`): **PASS**
- Staging-Datei bleibt liegen: **PASS** (kein `remove_file` im Fehlerpfad)
- attempts-Cap 8 → `tapes.status='failed'`: **PASS** (`tape_ingest.rs:280-292`)
- **Requeue über Claim-Backoff: FAIL.** `fail_upload` setzt den Job auf `state='failed'`
  (`store/tape_jobs.rs:440`), `claim` liest aber ausschließlich `state='pending'`
  (`store/tape_jobs.rs:166`) und die Lease-Recovery nur `state='running'`
  (`store/tape_jobs.rs:157`). Ein `failed`-Upload-Job wird **nie** wieder geclaimt →
  der Retry-Mechanismus greift nicht, der Job ist praktisch terminal. Der Requeue-Test T3
  umgeht das per manuellem `UPDATE … state='pending'` (`tests/tape_ingest.rs:381`) und belegt
  damit gerade **nicht** die geforderte Requeue-Semantik. Doc-Aussage in
  `docs/consolidated-architecture.md` („Der nächste Poll-Pass claimt den Job erneut") ist
  faktisch falsch.

#### 4. Idempotenz — PASS
`complete_upload` in einer Tx (`store/tape_jobs.rs:325`): `tapes`→`processing`, `probe`+`peaks`
per `ON CONFLICT DO NOTHING`, `upload`→`done`. `done` ist terminal (`transition_allowed`).

#### 5. Security — PASS mit Minor
- Path-Traversal: `staging_path` nutzt `root.join("{tape_id}.mp3")`; `tape_id` stammt aus
  server-generierter `tapes.id` (UUID), kein User-Input → akzeptabel.
- Token nur als `bearer_auth`: **PASS**. Keine Secrets in Logs: **PASS** (nur tape_id/job_id).
- Kein `unwrap` auf Env/IO im Produktionspfad: **PASS** (`from_env` → `.ok()?`).
- reqwest-Timeout: **MINOR** — nur `connect_timeout` (10 s, `tape_ingest.rs:126`);
  kein Request-/Read-Timeout → ein hängender Server blockiert den Ingest-Task unbegrenzt.
  Empfehlung: `.timeout(...)` zusätzlich.

#### 6. Worker-Guard — PASS
Claim lehnt `kind='upload'` mit 400 ab (`api/tape_jobs.rs:32`); PATCH auf Upload-Job → 409
`upload_job_not_patchable` (`store/tape_jobs.rs:230`). Test `ingest_jobs_are_not_worker_claimable_or_patchable` grün.

#### 7. Fail-open in main.rs — PASS
`spawn` gibt sofort `JoinHandle` zurück (detached), blockiert den Start nicht; ohne
Token/Base-URL (`from_env()==None`) sauberer No-Op mit Log (`main.rs:75-84`).

#### 8. Keine Fremdänderungen — PASS
Nur `scheduler/**` + `docs/**`.

### Fix-Anweisungen (Blocker)

1. **Requeue (Punkt 3):** In `fail_upload` den Job statt auf `failed` auf `pending` setzen,
   damit der bestehende Claim-Backoff (`attempts`/`updated_at`) greift:
   `UPDATE tape_jobs SET state='pending', error=?, updated_at=? WHERE id=?`
   (Transition `running→pending` ist erlaubt). Alternativ `claim` um `state='failed'` für
   `kind='upload'` erweitern. Danach T3 ohne manuelles Requeue-SQL testen und die
   `docs/consolidated-architecture.md`-Aussage korrigieren.
2. **Content-Length (Punkt 2):** vor `.body(body)` im PUT
   `.header(reqwest::header::CONTENT_LENGTH, size_bytes)` setzen (Wert liegt vor).
3. **Minor:** `reqwest::ClientBuilder::timeout(Duration::from_secs(...))` ergänzen.

Kein Code durch den Verifier geändert, kein Commit. Nächster Schritt: zurück an Developer (Fix 1 + 2).

## Developer-Fix-1-Report

Fix-Commit: `4f48b2eb54b412c384f0cf409be6554171c8d827`

### Behobene Verifier-Punkte

1. **Requeue (Blocker):** `fail_upload` setzt den Job jetzt auf `state='pending'`
   (inkl. `error=?, updated_at=?`), statt terminal `failed`. Der bestehende
   `claim`-Pfad (`state='pending'`) + `is_due(attempts, updated_at)` übernimmt den
   Requeue automatisch mit `attempts`-Backoff. Der Cap-Pfad
   (`attempts >= MAX_UPLOAD_ATTEMPTS`) bleibt terminal: neuer `terminal`-Parameter
   setzt `state='failed'` + `tapes.status='failed'`.
   T3 setzt kein `state='pending'` mehr manuell — nur `attempts=0`/`updated_at`
   altern, der echte Claim retried. T5 neu: Cap setzt Job+Tape terminal.
2. **Content-Length (Blocker):** PUT `/objects/{sha256}` setzt
   `.header(CONTENT_LENGTH, size_bytes)`; T1 prüft Header und Abwesenheit von
   `Transfer-Encoding`.
3. **Timeout (Minor):** reqwest-Client zusätzlich `.timeout(300s)`.

### Guard unverändert

`ingest_jobs_are_not_worker_claimable_or_patchable` grün: Upload-Jobs bleiben
weder claim- noch patchbar (400/409).

### Verifikation

- `cargo fmt --all` → sauber
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test --workspace` → exit 0, alle Binaries grün; `tape_ingest` 5/5
  (happy path, dedupe, requeue, unreachable, attempt cap)
