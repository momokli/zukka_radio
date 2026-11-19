# Plan: Auth (Issue #16, M2 — Portal)

Repo: `momokli/zukka_radio` · Branch: `feat/16-auth` · Basis/PR-Ziel: `v2`
Erstellt: 2026-11-19 · Planner: feature-dev-planner (Subagent)

## Scope

**Drin** (Auth-Akzeptanz aus `docs/portal.md` §7 „Auth" + §4.1/§4.3 SoT):

- Login-Konto (`POST /login`) mit Argon2id-Verifikation.
- Server-seitige Sessions (`sessions` in `zukka.db`) + Cookie
  `__Host-zukka_session` (`HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`, kein `Domain`).
- CSRF-Schutz für alle Nicht-GET-Routen (HMAC-Token + `Origin`/`Sec-Fetch-Site`).
- RBAC-Checks anhand der Permission-Matrix §6.10.1.
- Session-Rotation (bei Login und Passwort-/Rollenänderung), Logout.
- Getrenntes Login- vs. Stream-Passwort inkl. Ungleichheits-Prüfung (`422`).
- Login-Rate-Limit (5 Fehlversuche / 15 min / IP+Username → `429`).
- Minimales Portal-Gerüst (P0) nur so weit, wie Login / Dashboard / Account es brauchen.

**Draußen** (bewusst nicht in #16): Upload, Library, Queue, Schedule, Control-Panel,
Announcements, Admin-User-CRUD über die Akzeptanz hinaus.

## Architecture-Constraint (SoT, verbindlich)

- **Eine DB** `zukka.db`, **genau ein Writer = Scheduler**. Portal öffnet read-only;
  Session-Writes laufen über die Scheduler-API (`/internal/sessions`).
- **Scheduler = Identitäts-Autorität**: `casters`/`roles`/`sessions`, Argon2id,
  `POST /internal/auth/verify`, `POST/ PATCH/ DELETE /internal/sessions`.
- **Portal = dünne UI** (axum + askama + htmx): Login-Seite, Cookie, CSRF, RBAC-Checks,
  Logout. **Kein** eigenes JSON-API, **keine** eigenen Migrationen.
- Portal → Scheduler: `Authorization: Bearer $SCHEDULER_SERVICE_TOKEN`
  + `X-Caster-Id` + `X-Caster-Role`.
- Schema ist bereits vorhanden (`scheduler/migrations/0001_init.sql`:
  `roles`, `casters`, `sessions` mit `token_hash UNIQUE`, `csrf_secret`,
  `idle_expires_at < absolute_expires_at`, `revoked_at`). **Keine neue Migration nötig.**

## Betroffene Dateien

### Scheduler (Identitäts-Autorität)

- `Cargo.toml` — Workspace-Deps ergänzen: `argon2`, `rand`, `sha2`, `hmac`,
  `subtle`, `base64`.
- `scheduler/Cargo.toml` — o. g. Deps als workspace-Referenzen aufnehmen.
- `scheduler/src/store/mod.rs` — `pub mod auth;`.
- `scheduler/src/store/auth.rs` *(neu)* — casters/roles/sessions-Queries
  (Muster wie `store/queues.rs`: `sqlx::query_as`, `ApiError`, `now_ts`, `new_id`).
- `scheduler/src/api/auth.rs` — bestehende Liquidsoap-Token-Logik **erhalten**,
  um Interne-Auth-Handler erweitern (`verify`, `sessions`).
- `scheduler/src/api/mod.rs` — `/internal/auth/verify` + `/internal/sessions`
  (+ `/internal/sessions/{id}`) verdrahten; `/internal/casters/{id}` ergänzen.
  RBAC-Middleware bleibt TODO anderer Issues.
- `scheduler/src/dto.rs` — `CasterDto`, `RoleDto`, `SessionDto`, `VerifyRequest`,
  `VerifyResponse`, `CreateSessionRequest`, `SessionValidationDto`.
- `scheduler/src/error.rs` — `TooManyRequests` (`429`), `Forbidden` (`403`)
  und `Unauthorized`-Nutzung für Login-Fehler klarziehen.

### Portal (dünne UI)

- `portal/Cargo.toml` — `askama`, `askama_web` (oder eigener Wrapper),
  `argon2` **nicht** nötig (Verifikation bleibt im Scheduler), `sha2`, `hmac`,
  `subtle`, `base64`, `reqwest` (rustls), `tower-http`, `tower_governor`.
- `portal/src/main.rs` — Router: `/`, `/login`, `/logout`, `/account`,
  `/account/password`, `/account/stream-password`, `/healthz`; Middleware-Stack.
- `portal/src/config.rs` *(neu)* — Env: `PORTAL_BASE_URL` (Startup-Check auf `https`),
  `SCHEDULER_BASE_URL`, `SCHEDULER_SERVICE_TOKEN`, `SESSION_IDLE_DAYS=7`,
  `SESSION_ABSOLUTE_DAYS=30`.
- `portal/src/error.rs` *(neu)* — `PortalError` (P0) mit §4.6-Analogie für HTML
  (Fehlerseite statt Envelope), `403`/`401`-Mapping.
- `portal/src/api_client.rs` *(neu)* — typisierter Scheduler-Client (verify,
  create/validate/revoke session, change password, stream-password, caster get).
- `portal/src/auth/mod.rs` *(neu)* — Auth-Modul: `handlers`, `session`,
  `middleware`, `csrf`, `rate_limit`.
- `portal/src/auth/handlers.rs` *(neu)* — Login/Logout/Account-Handler.
- `portal/src/auth/session.rs` *(neu)* — Cookie-Bau/-Parse (`__Host-zukka_session`).
- `portal/src/auth/middleware.rs` *(neu)* — Session-Auflösung → RBAC-Kontext.
- `portal/src/auth/csrf.rs` *(neu)* — Token-Ausgabe + Nicht-GET-Verifikation.
- `portal/src/auth/rate_limit.rs` *(neu)* — Login-Rate-Limit (IP+Username).
- `portal/src/rbac.rs` *(neu)* — Permission-Guard (`require(permission)`), Matrix §6.10.1.
- `portal/templates/layout.html` *(neu)* — Grundgerüst, Nav, CSRF-`<meta>`,
  htmx-Header, vendored `htmx.min.js`, CSS.
- `portal/templates/login.html`, `dashboard.html`, `account.html` *(neu)*.

## User Stories (in Reihenfolge = Abhängigkeitsordnung)

### 1. Scheduler: Identitäts-Verifikation mit Argon2id

- **Akzeptanzkriterien:**
  - `POST /internal/auth/verify` (`{username, password}`) → `200 {caster}` mit
    `id`, `username`, `display_name`, `role` (Rollenname aus `roles`), `is_active`.
  - Argon2id-Verifikation mit per-User-Salt (Salt steckt im PHC-String `password_hash`).
  - Unbekannter User **und** falsches Passwort liefern identische `401`-Meldung und
    vergleichbare Antwortzeit (Blind-Verify gegen einen konstanten Dummy-Hash).
  - Inaktiver Caster (`is_active = 0`) → `401` (fail-closed).
  - Rate-limitierter Pfad → `429`.
  - Token-Auth für den Aufruf erzwingen (Service-Token), Endpoint nur intern.
- **Betroffene Dateien:** `store/auth.rs`, `api/auth.rs`, `dto.rs`, `error.rs`, `Cargo.toml`.
- **Geschätzte LoC:** ~230.

### 2. Scheduler: Session-Write-Through (create/validate/patch/delete)

- **Akzeptanzkriterien:**
  - `POST /internal/sessions` legt Session an: `id` = UUIDv7, `token_hash =
    sha256(token)` (Token nur atomar einmal zurück), `csrf_secret` = Zufall,
    `idle_expires_at = now+7d`, `absolute_expires_at = now+30d`, `last_seen_at = now`.
  - Validierung einer Session gegen `token_hash`: `revoked_at IS NULL`,
    `idle_expires_at > now`, `absolute_expires_at > now` → sonst ungültig.
  - `PATCH /internal/sessions/{id}` aktualisiert `last_seen_at` (Portal batcht ≤ 1×/min).
  - `DELETE /internal/sessions/{id}` setzt `revoked_at` (Logout).
  - „Alle anderen Sessions eines Users beenden" (bei Passwort-/Rollenänderung) vorhanden.
- **Betroffene Dateien:** `store/auth.rs`, `api/auth.rs`, `dto.rs`.
- **Geschätzte LoC:** ~240.

### 3. Portal: P0-Gerüst (askama + PortalError + CSRF-Meta + htmx)

- **Akzeptanzkriterien:**
  - `cargo run` liefert gestyltes `layout.html`; `layout.html` enthält
    `<meta name="csrf-token">` und setzt es global via `hx-headers` am `<body>`.
  - `PortalError` mit definierten 404/500-Seiten; `/healthz` vorhanden.
  - `PORTAL_BASE_URL`-Startup-Check auf `https` (bricht laut ab, wie in `portal.md` §5 gefordert).
  - Vendored htmx (genau eine JS-Dependency, D18).
- **Betroffene Dateien:** `main.rs`, `config.rs`, `error.rs`, `templates/*`, `Cargo.toml`.
- **Geschätzte LoC:** ~300.

### 4. Portal: Login-Flow + `__Host-zukka_session`-Cookie

- **Akzeptanzkriterien:**
  - `GET /login` rendert Formular (CSRF-Hidden-Feld).
  - `POST /login`: Rate-Limit (IP+Username) → Scheduler `/internal/auth/verify`
    → bei Erfolg `POST /internal/sessions` → Cookie setzen → `303 /`.
  - Cookie exakt `__Host-zukka_session` mit `HttpOnly`, `Secure`, `SameSite=Lax`,
    `Path=/`, **kein** `Domain`.
  - Falsches Passwort/unbekannter User → identische Meldung, gleiche Zeit;
    5 Fehlversuche/15 min → `429`.
- **Betroffene Dateien:** `auth/handlers.rs`, `auth/session.rs`, `auth/rate_limit.rs`,
  `api_client.rs`, `templates/login.html`.
- **Geschätzte LoC:** ~320.

### 5. Portal: Session-Middleware + RBAC-Guard (Permission-Matrix)

- **Akzeptanzkriterien:**
  - Middleware liest Cookie, validiert Session über den Scheduler und baut
    `AuthContext { caster_id, role, permissions, csrf_token, session_id }`.
  - Nicht authentifiziert: GET → Redirect `/login`; Nicht-GET → `401`.
  - `require(permission)` prüft **nur Permissions** (nie Rollen), permissiv nach §6.10.1.
  - `caster` erhält `403` auf `POST /control*` und `/admin/*`.
  - Ungültige/abgelaufene Session → `401` + Cookie löschen.
- **Betroffene Dateien:** `auth/middleware.rs`, `rbac.rs`, `api_client.rs`, `error.rs`.
- **Geschätzte LoC:** ~300.

### 6. Portal: CSRF-Schutz für Nicht-GET

- **Akzeptanzkriterien:**
  - Ausgeliefertes Token `HMAC(csrf_secret, "csrf")`; Portal vergleicht das
    mitgesendete Token konstantzeitlich (`subtle`) gegen das erwartete.
  - Zusätzlich `Sec-Fetch-Site: same-origin` bzw. vorhandener `Origin`-Header.
  - Nicht-GET ohne/mit falschem Token → `403`, **kein** Redirect — auch bei gültiger Session.
  - htmx sendet Token via `hx-headers`; klassische Formulare als Hidden-Feld.
- **Betroffene Dateien:** `auth/csrf.rs`, `auth/middleware.rs`, `templates/layout.html`.
- **Geschätzte LoC:** ~150.

### 7. Portal: Logout, Session-Rotation, Account-Passwörter

- **Akzeptanzkriterien:**
  - `POST /logout` (CSRF-geschützt) → `DELETE /internal/sessions/{id}`, Cookie löschen, `303 /login`.
  - Bei Login wird die Session rotiert (neues Token); bei Passwort-/Rollenänderung
    werden alle anderen Sessions invalidiert.
  - `POST /account/password`: setzt neues Login-Passwort über den Scheduler;
    Stream-Passwort == Login-Passwort wird in **beide Richtungen** serverseitig mit
    `422` abgelehnt (`argon2::verify`), inkl. Feldfehler.
  - `POST /account/stream-password`: optional generiertes 24-Zeichen-Passwort, nur
    **einmal** angezeigt, in der DB nur der Hash; laufende Harbor-Verbindung bleibt.
- **Betroffene Dateien:** `auth/handlers.rs`, `templates/account.html`,
  `api_client.rs`, Scheduler-`store/auth.rs` (Passwort-/Stream-Passwort-Writes).
- **Geschätzte LoC:** ~300.

## Datenflüsse

**Login**

```
Browser --GET /login--> Portal (rendert Form + CSRF-Meta)
Browser --POST /login (username,password,csrf)--> Portal
  Portal: Rate-Limit (IP+Username)
  Portal --POST /internal/auth/verify (Bearer service-token)--> Scheduler
    Scheduler: Argon2id verify (+ Blind-Verify bei unbekannt) -> {caster}
  Portal --POST /internal/sessions {caster_id}--> Scheduler
    Scheduler: INSERT sessions (sha256(token), csrf_secret, 7d/30d) -> {token, csrf_token}
  Portal --Set-Cookie __Host-zukka_session--> Browser (303 /)
```

**Geschützte Anfrage**

```
Browser --GET /dashboard (Cookie + csrf-Meta)--> Portal
  Portal: Cookie lesen -> Scheduler (Session-Validierung; liefert caster+permissions+csrf_token)
  Portal: RBAC require(permission) / CSRF-Check (Nicht-GET)
  Portal: Daten über Scheduler-API (Bearer + X-Caster-Id/-Role)
  Portal --> Browser (HTML/htmx-Fragment)
```

> **Offener Contract-Punkt (im PR zu klären):** Für die Middleware braucht das Portal
> eine Session-Lese-Route. `§4.2` listet nur `POST/PATCH/DELETE /internal/sessions`.
> Vorschlag: `GET /internal/sessions/{id}` (bzw. `.../current`) ergänzen, das
> `{caster, role, permissions, csrf_token}` liefert. Portal hält damit keinen
> Session-State — konsistent zu „nur der Scheduler schreibt/liest autoritativ".

## Teststrategie

**Scheduler (Unit, `#[cfg(test)]` bzw. `tests/`)**

- `argon2id`: Hash/Verify, per-User-Salt, Dummy-Hash-Blind-Verify-Matrix.
- Sessions: create → validate ok; abgelaufen (idle/absolute) → ungültig;
  revoked → ungültig; `last_seen_at`-Patch; „alle anderen revoken".
- CSRF-Token-Ableitung deterministisch für festes Secret.
- API-Tests mit Router + temporärer SQLite (Muster `tests/queue_crud.rs`):
  `auth_verify_api.rs`, `sessions_api.rs` (Statuscodes 200/401/403/422/429).

**Portal (Unit + Integration)**

- `csrf`-Vergleich (ok/falsch/fehlend), Cookie-Attribut-Builder.
- Rate-Limit-Fenster (5/15 min) inkl. Reset.
- RBAC-Guard-Matrix gegen §6.10.1 (caster/operator/admin × Permission).
- End-to-End (Reqwest gegen lokalen Portal-Testrouter + Fake-Scheduler oder echte DB):
  Login → Cookie → Dashboard `200`; fehlendes CSRF → `403`; `caster` → `403` auf
  `/control` & `/admin/*`; Logout revoked; Passwortänderung invalidiert Fremd-Sessions.

**CI-Gates (PR-Ziel `v2`)**

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- jeweils grün; `cargo fmt --all` vor jedem Push.

## Risiken / Notizen

- **Neue Workspace-Deps** (argon2/rand/hmac/base64/askama/reqwest/tower-*) berühren
  `Cargo.lock` — Build/CI auf `planet` gegenprüfen.
- **Contract-Lücke** „Session-Validierung GET" (s. o.) — klein halten, im PR dokumentieren.
- **`__Host-`** erzwingt HTTPS; lokale Dev-Auslieferung über `ZUKKA_EDGE_MODE`/Caddy
  sicherstellen, sonst bricht das Cookie bewusst laut (gewünscht).
- **Keine neue Migration** — `0001_init.sql` enthält alle benötigten Spalten;
  falls Erweiterungen nötig werden, gelten §3.1 (append-only, nur `scheduler/migrations`).
