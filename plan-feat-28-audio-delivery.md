# Plan: Issue #28 — Audio-Auslieferung `GET|HEAD /media/{slug}.{ext}` (M3)

- Repo: `momokli/zukka_radio`, Branch `feat/28-audio-delivery` (Basis `v2`)
- Source of Truth: `docs/consolidated-architecture.md` §4.5/§4.6, `docs/tapes.md` §3.5/§3.7
- Ist-Stand verifiziert: Route existiert nicht; `cors`-Middleware + `preflight` + ETag-Helfer + 405 sind vorhanden.
- Kein Code in diesem Dokument, nur Plan; Signaturen/Zustände beschreiben die Eingriffe.

---

## Betroffene Dateien

- `tapes/src/api/mod.rs` — Route `GET|HEAD /media/{file}` registrieren; `AppState` erweitern; `cors`-Middleware unverändert.
- `tapes/src/api/media.rs` — **neu**: Handler `get_media` (GET/Streaming) und `head_media` (Header ohne Body).
- `tapes/src/media.rs` — **neu**: reiner Range-Parser + `If-Range`-Prüfung (ohne HTTP, unit-testbar).
- `tapes/src/error.rs` — neue Varianten `RangeNotSatisfiable { size }` (416) und `UpstreamUnavailable` (502); Envelope-Antwort um Zusatz-Header erweitern.
- `tapes/src/store/tapes.rs` — `TapeRow` um `storage_key`, `etag` erweitern; `TAPE_SELECT` anpassen (DTO bleibt unberührt).
- `tapes/src/time.rs` — Helfer für Download-Dateinamen (Berliner Zeit).
- `tapes/src/main.rs` — `router(...)`-Aufruf an neue Signatur anpassen (Music-API-URL/Token durchreichen).
- `tapes/src/lib.rs` — `pub mod media;`.
- `tapes/tests/media_proxy.rs` — **neu**: Integrationstests (tower::oneshot) gegen gemockten Upstream.
- `tapes/tests/public_api.rs` — `setup()` an neue `router`-Signatur anpassen; Seed um `storage_key`/`etag` erweitern.
- (Deploy, separater Slice) `deploy/caddy/routes.caddy` — `/media/*` → `tapes:8080`, `flush_interval -1`, kein `encode`.

---

## Zentrale Entwurfsentscheidungen

- **HEAD wird nicht an den Upstream weitergereicht.** Axums automatisches HEAD (Body verwerfen) würde den ganzen Audiostream vom Upstream laden und wegwerfen — bei ~170 MB pro Tape inakzeptabel. Stattdessen eigener `head_media`-Handler, der alle Header lokal aus der `TapeRow` (mime, size_bytes, etag, Range-Parser) berechnet.
- **Route-Capture als ein Segment.** axum 0.8 erlaubt kein `{slug}.{ext}` innerhalb eines Segments. Route `/media/{file}`, Split mit `rsplit_once('.')`; fehlender Punkt → `404`.
- **Range-Parser ist pur** (keine I/O) in `src/media.rs`; der Handler entscheidet 206/200/416 nur anhand des Ergebnisses.
- **Upstream-416 wird normalisiert**: eigene `Content-Range: bytes */{size}` aus `tape.size_bytes`, nie die Upstream-Header ungeprüft durchreichen.
- **ETag-Quelle**: bevorzugt `tape.etag` (aus Objekt-Metadaten); Fallback `etag::for_tape(updated_at, slug)`, falls Spalte NULL.
- **CORS**: globaler `cors`-Middleware setzt bereits `Access-Control-Allow-Origin: *` auf jede Antwort; `preflight` liefert `204`. Der Media-Handler ergänzt nur `Access-Control-Expose-Headers` (gilt auch auf 206/304/416).

---

## User Stories (in Reihenfolge)

### 1. Datenzugriff + State-Plumbing (Fundament)
- **Ziel:** `TapeRow` trägt Proxy-Felder; Router kennt Music-API-URL/Token und einen geteilten HTTP-Client.
- **Akzeptanzkriterien:**
  - `public_tape()` liefert zusätzlich `storage_key: Option<String>` und `etag: Option<String>` (public DTO bleibt unverändert, Felder werden nie serialisiert — bestehender Test `…hides_internal_fields` bleibt grün).
  - `AppState` enthält `music_api_url: String`, `music_api_token: String`, `http: reqwest::Client` (z. B. gebündelt als `MediaProxy`-Struct).
  - `api::router(...)` nimmt diese Werte entgegen; `main.rs` speist sie aus `Config` (`music_api_url`/`music_api_token` existieren bereits) ein; `reqwest`-Client mit `stream`-Feature (bereits in Workspace-Deps).
- **Betroffene Dateien:** `store/tapes.rs` (`TapeRow`, `TAPE_SELECT`), `api/mod.rs` (`AppState`, `router`), `main.rs`, `tests/public_api.rs` (`setup()`).
- **Geschätzte LoC:** ~40.

### 2. Range-Parser + `If-Range` (`src/media.rs`)
- **Ziel:** Reine Funktionen, die die §3.7-Range-Matrix abbilden.
- **Signaturen:**
  - `enum ByteRange { Full, Satisfiable { start: u64, end: u64 }, Unsatisfiable }`
  - `fn parse_range(raw: Option<&str>, size: u64) -> ByteRange` — Regeln: fehlend → `Full`; Unit ≠ `bytes` (`items=`) → `Full`; Komma (Multi-Range, D10) → `Full`; `bytes=N-` mit N ≥ size → `Unsatisfiable`, sonst `end=size-1`; `bytes=-N` mit N=0 → `Unsatisfiable`, N ≥ size → ganz; `bytes=a-b` mit a > b oder a ≥ size → `Unsatisfiable`, `b` ≥ size → clampen; unparsbar → `Full`.
  - `fn if_range_allows(if_range: Option<&str>, etag: &str) -> bool` — fehlend → `true`; sonst starker Vergleich gegen `etag`.
- **Akzeptanzkriterien:** Unit-Tests (im Modul) für alle Zeilen der Matrix, inkl. `bytes=size-` → `Unsatisfiable`, `bytes=-0` → `Unsatisfiable`, `items=0-1` → `Full`, `bytes=0-1023,2000-3023` → `Full`.
- **Betroffene Dateien:** `src/media.rs` (neu), `src/lib.rs`.
- **Geschätzte LoC:** ~90 (inkl. Tests).

### 3. Error-Varianten 416 / 502 (+ Header)
- **Ziel:** §4.6-Envelope mit Zusatz-Headern.
- **Änderungen:**
  - `ApiError::RangeNotSatisfiable { size: u64 }` → `416`, `error.code = "range_not_satisfiable"`, zusätzlich Header `Content-Range: bytes */{size}`.
  - `ApiError::UpstreamUnavailable` → `502`, `error.code = "storage"`, zusätzlich Header `Retry-After: 5`.
  - `IntoResponse`-Aufbau so umbauen, dass nach dem Erzeugen der Envelope-Antwort varianteabhängig Header eingefügt werden (bestehende Varianten unverändert).
- **Akzeptanzkriterien:** 416 trägt `Content-Range: bytes */{size}` (Akzeptanz #11); 502 trägt `Retry-After: 5` und `error.code == "storage"` (§3.5).
- **Betroffene Dateien:** `error.rs`.
- **Geschätzte LoC:** ~35.

### 4. Download-Dateiname (`time.rs`)
- **Ziel:** `{caster}-{YYYY-MM-DD}-{HHMM}.{ext}` in Berliner Zeit.
- **Signatur:** `fn download_filename(caster: &str, started_at: DateTime<Utc>, ext: &str) -> String` — Formatierung über `BERLIN` (`%Y-%m-%d-%H%M`), DST-korrekt.
- **Akzeptanzkriterien:** `("momo", 2026-11-23T10:00:00Z, "mp3")` → `momo-2026-11-23-1100.mp3` (Akzeptanz #13).
- **Betroffene Dateien:** `time.rs`.
- **Geschätzte LoC:** ~15.

### 5. Handler + Route (Kern)
- **Ziel:** `GET|HEAD /media/{file}` inkl. Streaming, Cache-/CORS-Headern, 304, Download-Variante.
- **Route:** in `api/mod.rs` → `.route("/media/{file}", get(media::get_media).head(media::head_media).options(preflight))`.
- **Handler-Fluss `get_media` (`api/media.rs`):**
  1. `Path(file)` → `rsplit_once('.')` → `(slug, ext)`; ext lowercased ∈ `mp3|m4a|aac|ogg|opus|flac|wav`; sonst `TapeNotFound`. `ext` muss `row.format` entsprechen (case-insensitive) → sonst `404`.
  2. `store::tapes::public_tape` → `404` (byte-identisch, R9).
  3. ETag bestimmen (`tape.etag` oder `for_tape`-Fallback).
  4. `If-None-Match`-Treffer → `304` mit `ETag`, `Cache-Control`, `Accept-Ranges`, `Access-Control-Expose-Headers`.
  5. Range-Ermittlung: `if_range_allows` false → `Full`; sonst `parse_range`; `Unsatisfiable` → `ApiError::RangeNotSatisfiable { size }`.
  6. GET: Upstream `{music_api_url}/objects/{storage_key}` mit `bearer_auth(token)`; `Range`-Header nur weiterreichen, wenn 206 gewünscht (sonst ohne Range → volle Datei). Body per `Body::from_stream` streamen (kein Buffering).
     - Upstream `200`/`206` → Antwort-Header setzen: `Content-Type` = `tape.mime`, `Content-Length` aus Upstream (Fallback: berechnete Länge), `Content-Range` bei 206, `ETag`, `Cache-Control: public, max-age=31536000, immutable`, `Accept-Ranges: bytes`, `Access-Control-Expose-Headers: Content-Length, Content-Range, Accept-Ranges, ETag`; Status 206 nur wenn Upstream 206 **und** wir Range angefordert haben, sonst 200.
     - Upstream `416` → normalisiertes `416` (eigene `Content-Range`).
     - Netzwerkfehler / sonstige Nicht-Erfolge → `ApiError::UpstreamUnavailable`.
- **Handler `head_media`:** identische Vorstufen 1–5, dann Antwort **ohne** Upstream-Aufruf: `Content-Length` (Range- oder volle Größe), `Content-Type`, `ETag`, `Content-Range` bei Range, `Accept-Ranges`, `Cache-Control`, Expose-Header; leerer Body.
- **`?download=1`:** `RawQuery` parsen; setzt `Content-Disposition: attachment; filename="{download_filename(...)}"` und überschreibt `Cache-Control` mit `private, no-transform`. Gilt für GET und HEAD.
- **Akzeptanzkriterien:** Akzeptanz #10–#14; CORS-Expose-Header auf jeder 200/206; `Content-Encoding` fehlt.
- **Betroffene Dateien:** `api/media.rs` (neu), `api/mod.rs`, `lib.rs`.
- **Geschätzte LoC:** ~180.

### 6. Integrationstests gegen gemockten Upstream
- **Ziel:** End-to-End über `tower::oneshot` ohne Netz.
- **Aufbau:** In `tests/media_proxy.rs` einen Mini-Upstream-axum-Server an `127.0.0.1:0` starten (liest `Range`, antwortet 200/206/416 mit `Content-Range`/`Content-Length`/`ETag`); dessen `addr` als `music_api_url` in den `AppState`/`MediaProxy` geben. Erweiterter Seed: Tapes mit `storage_key` + `etag` (z. B. `aaa111` → `recordings/aaa111.mp3`, ETag `"abc"`).
- **Fälle:**
  - `200`: `Accept-Ranges: bytes`, `Content-Type`, `Content-Length`, `ETag`, `Cache-Control: …immutable`, `Access-Control-Allow-Origin: *`, Expose-Header.
  - `Range: bytes=0-1023` → `206` + `Content-Range: bytes 0-1023/{size}` + `Content-Length: 1024`.
  - `bytes=-4096` → `206`/4096; `bytes={size}-` → `416` + `Content-Range: bytes */{size}`; `bytes=0-99,200-299` → `200`; `items=0-1` → `200`.
  - `If-Range` passend → `206`, nicht passend → `200`; `If-None-Match` passend → `304` leer.
  - `HEAD` → gleiche Header, leerer Body, **kein** Upstream-Call (Mock zählt Requests).
  - `?download=1` → `Content-Disposition: attachment; filename="Alpha-2026-11-23-1100.mp3"`, `Cache-Control: private, no-transform`.
  - ext ∉ Format → `404`; unbekannt/`hidden1`/`pending1` → `404` byte-identisch; Upstream down → `502` + `Retry-After: 5`; Upstream-416 → unser `416`; `OPTIONS` → `204`; `POST /media/...` → `405` mit `Allow: GET, HEAD, OPTIONS`.
- **Betroffene Dateien:** `tests/media_proxy.rs` (neu), `tests/public_api.rs` (Seed + `setup()`), ggf. kleiner Test-Helfer für den Mock.
- **Geschätzte LoC:** ~250.

### 7. Deployment-Slice (Caddy)
- **Ziel:** `/media/*` passthrough ohne Kompression/Buffering.
- **Akzeptanzkriterien:** Route `tapes.<dom> /media/* → tapes:8080` mit `flush_interval -1`; `encode` wird per `@media not path /media/*` gescoped (Akzeptanz #14).
- **Betroffene Dateien:** `deploy/caddy/routes.caddy`.
- **Geschätzte LoC:** ~10 (Konfig, kein Rust).

---

## Implementierungs-Reihenfolge

1. **Story 1** (Datenzugriff/State) — Voraussetzung für alles.
2. **Story 2** (Range-Parser) + **Story 4** (Filename) — pur, parallel, ohne Abhängigkeit zu 1.
3. **Story 3** (Error-Varianten) — von Story 5 genutzt.
4. **Story 5** (Handler + Route) — der Kern.
5. **Story 6** (Integrationstests) — verifiziert 1–5.
6. **Story 7** (Caddy) — Deploy-Schritt nach grünem CI.

**Abhängigkeiten:** 1 → 5; 2,3,4 → 5; 5 → 6; 5 → 7.

---

## Teststrategie (Details)

- **Unit:** `src/media.rs` `#[cfg(test)]` für `parse_range`/`if_range_allows` (volle Matrix, Boundary `size`, `size-1`, `size`, `size+1`).
- **Integration:** `tests/media_proxy.rs` via `tower::ServiceExt::oneshot` gegen `api::router(...)` mit temp SQLite (Muster aus `tests/public_api.rs`) und lokalem Upstream-Mock an Ephemeral-Port. Header-/Status-Matrix wie Story 6; Mock zählt Requests, um „HEAD macht keinen Upstream-Call" zu belegen.
- **Regression:** bestehende `tests/public_api.rs` müssen nach Signatur-/Seed-Anpassung unverändert grün bleiben (insbesondere `hides_internal_fields`, `pool_is_truly_read_only`).
- **CI:** `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` (M0-Standard).

---

## Risiken / Offene Punkte

- **Upstream ohne `Content-Length`/`ETag`** (tapes.md F4/R3): Fallback über `tape.size_bytes`/`tape.etag`; Range lokal rekonstruierbar. Falls der Spike (F4) fehlt, im Test-Mock beide Varianten abdecken.
- **SHA/dep:** `futures-util` für `TryStreamExt`/`Body::from_stream` ist bereits Dependency.
- **Routing-405:** darauf achten, dass axum mit `.head(...)` `Allow: GET, HEAD, OPTIONS` liefert (Akzeptanz #16).
- **Bandbreite** (R2): `immutable`-Caching + Caddy-`flush_interval -1`; kein Buffering im Handler.
