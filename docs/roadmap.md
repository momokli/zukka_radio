# Roadmap — radio zukka v2

Quelle: [`consolidated-architecture.md`](consolidated-architecture.md).

## Milestones

| # | Milestone | Ziel |
| --- | --- | --- |
| M0 | **Foundation** | Repo, Env, Compose, Liquidsoap-Skeleton, Caddy, DNS |
| M1 | **Scheduler Core** | DB, Queue, Schedule, Prioritäten, Playback-API |
| M2 | **Portal** | Auth, Upload, Library, Queue-UI, Control Panel |
| M3 | **Tapes** | öffentlicher Feed, Waveform-Pipeline |
| M4 | **Multi-Caster** | Harbor-Pool, Live-Auswahl, Handoff |
| M5 | **Announcements** | Templates, Events, Telegram |
| M6 | **Deployment** | CI/CD, Deploy-Gate, Backup, Monitoring |
| M7 | **Migration** | PROD-Cutover |

---

## M0 — Foundation

- [ ] **Repo-Skeleton**: Cargo-Workspace mit `scheduler/`, `portal/`, `tapes/`, `liquidsoap/`, `deploy/`, `env/`.
- [ ] **Env-Schema**: `env/dev.env.example` + `env/prod.env.example`, identischer Schlüsselsatz.
- [ ] **Docker Compose**: `compose/dev.yml` + `compose/prod.yml` mit 6 Services, 6 Volumes, 3 Netzen.
- [ ] **Liquidsoap-Skeleton**: `radio.liq` + `lib/` (harbor, live, layers, master, outputs, control, events).
- [ ] **Caddy**: `routes.caddy` + `Caddyfile.dev` + `Caddyfile.prod`.
- [ ] **DNS**: `*.dev.zukkafabrik.de` → `65.21.27.234` (Cloudflare, DNS-only).

## M1 — Scheduler Core

- [ ] **SQLite-Schema**: 17 Tabellen, `STRICT`, UUIDv7, WAL.
- [ ] **Migrationen**: `scheduler/migrations/`, `sqlx::migrate!`.
- [ ] **UUIDv7**: Generator (zeit-sortierbar).
- [ ] **Queue-CRUD**: `/queues`, `/queues/{id}/items`, `/reorder`, `/clear`.
- [ ] **Schedule/Broadcast-CRUD**: `/shows`, `/broadcasts`, `/schedule`, `/schedule/expand`.
- [ ] **Sources/Prioritäten**: 6 Layer, Defaults 100/80/60/40/20/1.
- [ ] **Playback-API**: `/playback/next?layer=`, `/playback/current`, `/playback/started|ended|error`.
- [ ] **Rotation-M3U**: `/playback/rotation.m3u` mit Music-API-URLs.
- [ ] **Control-API**: `/control/live`, `/control/switch`, `/control/skip`, `/control/pause`.

## M2 — Portal

- [ ] **Auth**: Login, Sessions (`__Host-zukka_session`), CSRF, Argon2id.
- [ ] **Caster-Passwort**: separat vom Login, serverseitig erzwungen.
- [ ] **Upload**: Multipart → Temp → ffprobe → Music API → `upload_jobs`.
- [ ] **Library**: gemeinsame + private Tracks, Suche, Filter.
- [ ] **Queue-UI**: pro Caster, hinzufügen/umsortieren/skippen, Links.
- [ ] **Schedule/Calendar**: Broadcasts planen, Shows, Wiederholung.
- [ ] **Control Panel**: SSE, Live-State, Umschalten, Pause/Skip.
- [ ] **Waveforms**: Anzeige in Library und Queue.

## M3 — Tapes

- [ ] **Public API**: `/api/v1/tapes`, `/api/v1/tapes/{slug}`, `/api/v1/casters`.
- [ ] **Waveform-Pipeline**: ffmpeg-Streaming-Decode → RMS-Peaks → `ZKWV`.
- [ ] **Tape-Seite**: Retro/Pixel-Optik, pro Caster, Filter, Player.
- [ ] **Share-Links**: `/t/{slug}`, OG-Meta.
- [ ] **Audio-Auslieferung**: Range-Requests, CORS.

## M4 — Multi-Caster

- [ ] **Harbor-Pool**: 32 Slots, `input.harbor` pro Slot.
- [ ] **Live-Auswahl**: FCFS über `on_connect`/`on_disconnect`.
- [x] **Handoff**: nahtlos, 0,5-s-Fade.
- [ ] **Control**: `live.set`, `live.clear`, `live.status`.
- [ ] **Harbor-Auth**: `POST /internal/harbor/auth` (Scheduler, < 200 ms).

## M5 — Announcements

- [ ] **Templates**: Platzhalter (`{show_name}`, `{caster_name}`, …).
- [ ] **Events**: live on/off, Show-Reminder, Recording veröffentlicht, Fehler.
- [ ] **Timings**: global + pro Show.
- [ ] **Telegram**: Bot-Integration, ein Haupt-Channel.
- [ ] **Manuell**: Caster können posten.

## M6 — Deployment

- [ ] **CI**: fmt, clippy, test, sqlx, liquidsoap --check, caddy validate, compose config, gitleaks.
- [ ] **Deploy DEV**: Push auf `v2`.
- [ ] **Deploy PROD**: Tag/Release, Deploy-Gate.
- [ ] **Deploy-Gate**: `/internal/ops/audience`, 0 Hörer oder 03:00–04:00.
- [ ] **Backup**: `sqlite3 .backup`, restic, Restore-Drill.
- [ ] **Monitoring**: Prometheus, Blackbox, Alerts.

## M7 — Migration

- [ ] **PROD-Cutover**: DNS, Caddy, Compose.
- [ ] **Datenmigration**: Tracks, Tapes, Caster.
- [ ] **Rollback-Plan**.
