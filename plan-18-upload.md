# Plan — Issue #18 "Upload" (M2 Portal)

Repo: momokli/zukka_radio · Branch: `feature/18-upload` · Basis: `origin/v2` (7efece6)
Spec: `docs/consolidated-architecture.md` §4.2/§4.3, P2/P3/P8; `docs/portal.md` D6/D7/D8, §7.

## Ziel

Upload-Pipeline: **Multipart → Temp (Streaming) → ffprobe → ggf. Transcode (D7) →
Music API → `upload_jobs`**. Akzeptanz: Streaming ohne OOM, Job neustartfest,
SHA-256-Dedupe, webm/mp4-Videospur → abspielbares Derivat, Master bleibt erhalten.

Leitplanken aus der Spec:
- **D6:** Nie im RAM. Multipart-Stream auf Temp-Datei, harte Größenlimits (`UPLOAD_MAX_BYTES`).
- **D7:** Master (Original) unverändert; Derivat **MP3 320 CBR, 44,1 kHz, stereo** nur wenn
  Codec nicht in `{mp3, flac, vorbis, opus, aac, alac, pcm_s16le/s24le, aiff}` **oder** eine
  Videospur vorhanden ist.
- **D8:** Waveform-Peaks (`ffmpeg` → mono 8 kHz PCM → 800 Min/Max-Buckets, 8 bit) als JSON.
- **P2:** Nur der Scheduler schreibt; Portal schreibt per `/internal/*` (Write-Through).
- **P3:** UUIDv7 als TEXT-PK, kanonisch lowercase (36 Zeichen).
- **P8:** Kein Delay-Pfad berührt; Upload ist asynchron über Jobs.

---

## 1. User Stories / Arbeitseinheiten

Reihenfolge = Abhängigkeitsreihenfolge. Jede Einheit ist einzeln baubar und testbar.

### US-1 — Upload-Job-Store (Scheduler)
- **Als** Scheduler **will ich** `upload_jobs` anlegen/lesen/aktualisieren,
  **damit** Job-Status Neustarts überlebt.
- **Akzeptanz:** `create` (state=pending, UUIDv7, `created_at`/`updated_at` UTC ms),
  `get`, `patch_state` (pending→running→done/failed, setzt `sha256`, `track_id`, `error`,
  inkrementiert `attempts`), `find_by_hash` (Dedupe), `list_stale_running(older_than)`.
- **Dateien:** `scheduler/src/store/upload_jobs.rs` (neu), `scheduler/src/store/mod.rs` (+mod).
- **LoC:** ~180.

### US-2 — Write-Through-Endpunkte `/internal/upload-jobs` (Scheduler)
- **Als** Portal **will ich** Jobs per HTTP anlegen und patchen,
  **damit** der Scheduler einziger Writer bleibt (P2).
- **Akzeptanz:**
  - `POST /api/v1/internal/upload-jobs` → `201 {id}` (Body: `caster_id`, `filename`,
    `size_bytes`, optional `sha256`). 422 bei fehlenden Pflichtfeldern.
  - `PATCH /api/v1/internal/upload-jobs/{id}` → `200` (Body: `state`, `sha256?`,
    `track_id?`, `error?`). 404 unbekannt, 409 unerlaubter Übergang.
  - Beide hinter `require_service_token` (bereits vorhanden, `api/auth.rs`).
- **Dateien:** `scheduler/src/api/upload_jobs.rs` (neu), `scheduler/src/api/mod.rs` (Route in
  `internal`-Router), `scheduler/src/dto.rs` (+DTOs).
- **LoC:** ~140.

### US-3 — `MusicApi`-Trait + HTTP-Client + Fake (Portal)
- **Als** Portal **will ich** die Music API hinter einem Trait kapseln,
  **damit** ein abweichender Contract (Q1) nur eine Datei betrifft und Tests ohne Netz laufen.
- **Akzeptanz:** Trait `MusicApi` (`create_track`, `upload_asset`, `get_track`, `search`,
  `find_by_hash`, `delete`) wie `docs/portal.md` §3.5; `HttpMusicApi` (reqwest, Bearer
  `MUSIC_API_TOKEN`, Timeout); `MusicApiError`; `FakeMusicApi` im `dev`-Kontext für Tests.
- **Dateien:** `portal/src/music_api.rs` (neu), `portal/src/lib.rs` (+mod).
- **LoC:** ~260 (inkl. Fake).

### US-4 — Upload-Job-Client (Portal → Scheduler)
- **Als** Portal **will ich** Job anlegen/patchen können,
  **damit** der Upload-Flow Write-Through nutzt.
- **Akzeptanz:** Erweiterung analog `IdentityApi`: `UploadJobApi`-Trait mit `create_job`,
  `patch_job`; `HttpUploadJobApi` (`/api/v1/internal/upload-jobs`, Bearer Service-Token).
- **Dateien:** `portal/src/api_client.rs` (erweitern).
- **LoC:** ~120.

### US-5 — Streaming-Multipart-Handler + Temp-Datei (Portal)
- **Als** Caster **will ich** große Dateien hochladen,
  **damit** der Server nicht OOMt.
- **Akzeptanz:** `POST /library/upload` (RBAC `library.upload`, CSRF aktiv). Multipart-Felder
  werden **stückweise** auf eine Temp-Datei (`tempfile`) geschrieben, nie vollständig gepuffert.
  Überschreitet die Summe `UPLOAD_MAX_BYTES` → `413`, Temp wird gelöscht. SHA-256 wird beim
  Schreiben inkrementell berechnet. Mehrere Dateien pro Request erlaubt.
- **Dateien:** `portal/src/upload.rs` (neu), `portal/src/app.rs` (Route + AppState-Felder).
- **LoC:** ~220.

### US-6 — ffprobe-Integration (Portal)
- **Als** Pipeline **will ich** Codec/Container/Streams kennen,
  **damit** D7 entschieden werden kann.
- **Akzeptanz:** `ffprobe` als Subprozess (`FFPROBE_BIN`, Default `ffprobe`) mit
  `-print_format json -show_streams -show_format`; geparste Felder: `codec_name`,
  `codec_type`, `duration` (ms), `bit_rate`, `streams[]` (Video-Erkennung). Nicht-zero-Exit
  oder unparsebares JSON → Job `failed` mit Grund.
- **Dateien:** `portal/src/ffprobe.rs` (neu), `portal/src/lib.rs` (+mod).
- **LoC:** ~130.

### US-7 — Transcode-Regel D7 + Derivat (Portal)
- **Als** Pipeline **will ich** nur bei Bedarf ein Derivat erzeugen,
  **damit** Master erhalten bleibt.
- **Akzeptanz:** Reine Entscheidungsfunktion `needs_derivate(codec, has_video) -> bool`.
  Bei `true`: `ffmpeg` → MP3 320 CBR 44,1 kHz stereo in zweite Temp-Datei. `false`: kein
  zweiter Prozess. Master-Temp bleibt in jedem Fall zum Upload erhalten.
- **Dateien:** `portal/src/transcode.rs` (neu), `portal/src/lib.rs` (+mod).
- **LoC:** ~150.

### US-8 — Pipeline-Orchestrierung + Dedupe (Portal)
- **Als** Pipeline **will ich** den Ablauf steuern,
  **damit** Dedupe und Musik-Upload korrekt verschränkt sind.
- **Akzeptanz (Reihenfolge):**
  1. `create_job` (pending) über `UploadJobApi`.
  2. Stream → Temp, SHA-256 berechnen.
  3. `find_by_hash(sha256)`: Treffer → Job `done` mit vorhandener `track_id`, kein 2. Track.
  4. `ffprobe` → D7-Entscheidung → ggf. Derivat.
  5. `upload_asset` + `create_track` an Music API.
  6. `patch_job` → `done` + `track_id`; Fehler → `failed` + `error`.
- **Dateien:** `portal/src/upload_pipeline.rs` (neu).
- **LoC:** ~230.

### US-9 — Job-UI mit Polling (Portal)
- **Als** Caster **will ich** den Status sehen,
  **damit** ich weiß, ob der Upload fertig ist.
- **Akzeptanz:** `GET /library` (Liste) und Job-Fragment `POST/GET /library/upload` htmx:
  `hx-get` Fragment alle 1 s solange `pending`/`running` (portal.md §3.3 `upload-job`),
  zeigt State/Fehler/Fortschritt. Templates `library.html`, `library_upload.html`,
  `partials/upload_job.html`.
- **Dateien:** `portal/src/upload.rs` (Fragment-Handler), `portal/src/templates.rs`,
  `portal/templates/{library.html,library_upload.html}`, `portal/templates/partials/upload_job.html`,
  `portal/templates/layout.html` (Nav-Link).
- **LoC:** ~180.

### US-10 — Neustart-Sicherheit / Recovery (Scheduler + Portal)
- **Als** Betreiber **will ich**, dass kein Zombie-`running`-Job bleibt,
  **damit** Abbruch/Neustart sauber ist.
- **Akzeptanz:** Beim Scheduler-Start Recovery-Task: `running`-Jobs älter als
  `UPLOAD_RUNNING_TIMEOUT` (Default 30 min) → `failed` (Grund `timeout`), oder Requeue wenn
  `attempts < max`. Portal re-triggert nach Neustart keine Doppel-Dedupe (über `sha256`).
- **Dateien:** `scheduler/src/store/upload_jobs.rs` (`list_stale_running`),
  `scheduler/src/recovery.rs` (neu) oder Start-Hook `scheduler/src/main.rs`.
- **LoC:** ~90.

---

## 2. Exakte Dateiliste (neu/geändert)

| Datei | Neu/Änd | Verantwortlichkeit |
| --- | --- | --- |
| `scheduler/src/store/upload_jobs.rs` | neu | CRUD + Dedupe-Lookup + stale-running (US-1, US-10) |
| `scheduler/src/store/mod.rs` | änd | `pub mod upload_jobs;` |
| `scheduler/src/api/upload_jobs.rs` | neu | `POST`/`PATCH /internal/upload-jobs` (US-2) |
| `scheduler/src/api/mod.rs` | änd | Routen in `internal`-Router |
| `scheduler/src/dto.rs` | änd | `CreateUploadJobRequest`, `PatchUploadJobRequest`, `UploadJobDto` |
| `scheduler/src/main.rs` | änd | Recovery-Task beim Start (US-10) |
| `portal/src/music_api.rs` | neu | `MusicApi`-Trait, `HttpMusicApi`, `FakeMusicApi`, `MusicApiError` (US-3) |
| `portal/src/api_client.rs` | änd | `UploadJobApi`-Trait + `HttpUploadJobApi` (US-4) |
| `portal/src/upload.rs` | neu | Multipart-Streaming-Handler + Job-Fragment (US-5, US-9) |
| `portal/src/ffprobe.rs` | neu | ffprobe-Subprozess + Parser (US-6) |
| `portal/src/transcode.rs` | neu | D7-Entscheidung + ffmpeg-Derivat (US-7) |
| `portal/src/upload_pipeline.rs` | neu | Orchestrierung + SHA-256-Dedupe (US-8) |
| `portal/src/app.rs` | änd | AppState (`music`, `uploads`, `max_bytes`, Temp-Dir), Routen |
| `portal/src/config.rs` | änd | `MUSIC_API_URL`, `MUSIC_API_TOKEN`, `UPLOAD_MAX_BYTES`, `FFPROBE_BIN`, `FFMPEG_BIN`, `UPLOAD_TMP_DIR` |
| `portal/src/templates.rs` | änd | `LibraryPage`, `LibraryUploadPage`, `UploadJobFragment` |
| `portal/src/lib.rs` | änd | neue Module |
| `portal/templates/library.html` | neu | Library-Liste |
| `portal/templates/library_upload.html` | neu | Upload-Form (htmx, multipart) |
| `portal/templates/partials/upload_job.html` | neu | Job-Status-Fragment (Polling) |
| `portal/templates/layout.html` | änd | Nav-Link Library |
| `portal/tests/upload_pipeline.rs` | neu | Integration mit `FakeMusicApi` + FakeJobApi (US-5/6/7/8) |
| `portal/tests/ffprobe_transcode.rs` | neu | Unit-Tests Parser + D7-Matrix (US-6/7) |
| `scheduler/tests/upload_jobs_api.rs` | neu | Endpunkt- + Store-Tests (US-1/2/10) |

Betroffene, **nicht** zu ändernde Dateien: `portal/src/error.rs` (nur nutzen; ggf. `413`-Variante
ergänzen → als kleine Änderung in `error.rs` erlaubt, `PayloadTooLarge`).

---

## 3. Teststrategie

Grundsatz: alles Netz-/Disk-nahe ist austauschbar (Traits + Fakes), Tests laufen ohne
Music API und ohne echten Scheduler.

### Unit
- **US-1:** `upload_jobs`-Store gegen In-Memory-SQLite (Muster `scheduler/tests/auth_store.rs`):
  create/get/patch_state/find_by_hash, `attempts`-Inkrement, stale-running-Filter.
- **US-6:** `ffprobe`-JSON-Parser gegen gespeicherte Fixtures (mp3, flac, webm mit Video,
  mp4 mit Videospur, defektes JSON).
- **US-7:** Tabellentest der D7-Matrix: jedes akzeptierte Codec-ohne-Video → `false`; jeder
  nicht-akzeptierte Codec **oder** Videospur → `true`.

### Integration
- **US-2:** `POST`/`PATCH` gegen Scheduler-Router + In-Memory-DB; 201/200/404/409/422;
  Service-Token-Middleware greift (Muster `scheduler/tests/*_api.rs`).
- **US-3/4:** `HttpMusicApi`/`HttpUploadJobApi` gegen `wiremock`-nahe Stub (oder Mockito-frei
  via Axum-Testrouter) — Statuscode→Error-Mapping.
- **US-5:** Multipart-Streaming mit großem Body-Chunk (generierter ≥ 200 MB-Body als
  Streaming-Reader, nicht im Test-RAM allokiert) → Temp-Datei korrekt, keine RAM-Spitze
  (Body wird als `Body::from_stream` gefüttert); Überlimit → `413` + Temp gelöscht.
- **US-8:** Vollständige Pipeline mit `FakeMusicApi` + Fake-`UploadJobApi` + Test-Fixture-Datei:
  - Dedupe: zweiter Upload gleicher Bytes → `find_by_hash`-Treffer → kein 2. `create_track`.
  - webm/mp4 mit Videospur → Derivat erzeugt, `create_track` mit Master + Derivat.
  - ffprobe-Fehler → Job `failed` mit `error`.
- **US-9:** htmx-Fragment liefert `pending`/`running`-Polling-Markup; `done`/`failed` rendert Status.

### Akzeptanzkriterien → Nachweis
| AC (`portal.md` §7) | Nachweis |
| --- | --- |
| 200-MB-FLAC streaming, kein RAM-Peak | US-5 Integrationstest mit Streaming-Body |
| webm/mp4 mit Videospur → Derivat, Master bleibt | US-6/7 + US-8 Integrationstest |
| Doppel-Upload → kein 2. Track (Dedupe) | US-1 `find_by_hash` + US-8 Test |
| Abbruch/Neustart → kein Zombie-`running` | US-10 Recovery-Test + Timeout-Filter |
| Waveform ohne manuellen Schritt | US-8 Test (Job triggert Waveform-Schritt; D8) |

---

## 4. Risiken / Offene Fragen

- **Q1 — Music-API-Contract unbestätigt** (Upload-Mechanismus, Hash-Lookup, Abspiel-URL).
  → **Mitigation:** `MusicApi`-Trait genau nach `docs/portal.md` §3.5 definieren; Fake + HTTP
  austauschbar; Trait ist die einzige Kontaktfläche (D7-Spec sagt: abweichender Contract
  betrifft nur eine Datei).
- **ffmpeg/ffprobe im Container** vorhanden? → Config-basiert (`FFPROBE_BIN`/`FFMPEG_BIN`),
  Startup-Health-Check, Job `failed` mit klarer Meldung wenn Binärdatei fehlt.
- **Temp-Disk-Budget** (Q10): `UPLOAD_MAX_BYTES` + Disk-Guard vor Annahme; Temp-Cleanup bei
  Fehler und am Job-Ende; Retention-Vorschlag Temp 24 h (Q10 offen → nicht blockierend).
- **CPU-Last Transcode** sprengt Stream (Risiko §5 in portal.md): Worker-Pool = 2, `nice`,
  Backpressure. Scope für #18: sequentielle Ausführung pro Request; Rate-Limit-Feintuning später.
- **Parallel-Multipart** und Teil-Uploads: für #18 bewusst nicht (kein Chunked/Resume).
- **Portal-RAM beim Streaming:** muss über `axum::extract::Multipart` mit inkrementellem
  Field-Polling erfolgen (Tokio; couch: `multipart`-Feature in axum).
- **Neustart mid-Upload:** Portal-seitiger Temp wird verwaist; Cleanup-Task beim Start
  (Muster `UPLOAD_TMP_DIR`-Scan) — klein halten.

---

## 5. Build/Test-Kommandos

Vor jedem Push: `cargo fmt --all` (CI verlangt `cargo fmt --all -- --check`).

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p zukka-scheduler
cargo test -p zukka-portal
cargo test --workspace
```

Hinweis: schwere Builds auf `planet` laufen lassen; `cargo test --workspace` deckt Scheduler +
Portal ab. Fixtures für ffprobe (kleine Probe-JSONs) und ein kleines Audio-Test-Asset für die
Pipeline-Integrationstests unter `portal/tests/fixtures/` ablegen.

---

## 6. Implementierungs-Reihenfolge (Kurzfassung)

1. US-1 (Store) → 2. US-2 (Scheduler-Endpunkte) → 3. US-3 (MusicApi-Trait/Fake) →
4. US-4 (Job-Client) → 5. US-6 (ffprobe) → 6. US-7 (D7/Transcode) → 7. US-5 (Multipart/Stream) →
8. US-8 (Orchestrierung/Dedupe) → 9. US-9 (Job-UI) → 10. US-10 (Recovery).
Tests jeweils mit der Einheit; Integration (US-8) am Ende.
