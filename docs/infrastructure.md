# Infrastruktur & Deployment

## 1. Scope

### 1.1 Ziel

Dieses Dokument ist der Detail-Plan für **Betrieb, Deployment und Ops** des
radio-zukka-v2-Stacks: Env-Schema, Docker Compose, Caddy, systemd, CI/CD,
Deployment-Fenster, Backup/Restore, Monitoring/Healthchecks.

Grundsätze aus [`architecture.md`](architecture.md), die hier technisch erzwungen
werden:

| Grundsatz                                        | Technische Umsetzung in diesem Dokument                                                                                                                                                |
| ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Zero Downtime für Queue/Schedule/Live-Änderungen | Audio-Pfad (icecast + liquidsoap) ist unabhängig vom Control-Plane-Pfad (scheduler/portal/tapes). `caddy reload` ohne Restart; Fenster-Policy für alle Stateful-Services (§2.9).       |
| Berlin-Zeit überall                              | `TZ=Europe/Berlin` in jedem Container **und** systemd-Units; Zeitzone in systemd-Timern explizit. Persistenz intern UTC (Contract der Data-Model-Doku), Ausgabe immer `Europe/Berlin`. |
| Music API = Datei-Autorität                      | Kein Track/Recording-Verzeichnis als „Wahrheit" im Stack. Nur Caches/Staging mit Retention (§2.4).                                                                                     |
| RBAC von Anfang an                               | Jeder interne HTTP-Aufruf trägt ein Service-Token; kein Service-Port ist öffentlich erreichbar (§2.3).                                                                                 |
| Kein Delay                                       | Kein Buffer im Proxy-Pfad (`flush_interval -1`), keine Transcoding-Stufe im Edge, HLS optional (§2.5).                                                                                 |

### 1.2 Nicht-Scope

- Applikations-Interna (Endpunkte, Schema, Liquidsoap-Script, UI) — siehe
  `data-model.md`, `liquidsoap.md`, `portal.md`, `tapes.md`.
- Deploy/Provisionierung der externen Autoritäten (music-api, music-hub).
- Aufbau/Änderung des Shared Edge und der geteilten Monitoring-Stacks über die in
  §6 Phase 1 genannten minimalen Eingriffe hinaus.
- Abschaltung des aktuell auf dem Host laufenden Radio-Bestandssystems — nur die
  Abhängigkeit davon (§4, Frage 2) ist hier dokumentiert.

### 1.3 Faktenlage (read-only Bestandsaufnahme, 2026-11-18)

Alle Punkte unten wurden per SSH auf `cast.zukkafabrik.de` **nur lesend** erhoben
(`uname`, `/etc/os-release`, `docker ps`, `ss -lnt`, `docker inspect`,
`systemctl list-unit-files`, `systemctl cat`, `sed` auf Configs).

| Fakt           | Wert                                                                                                                                                                                                                                                                         | Quelle                                                       |
| -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Host           | `planet`, Ubuntu 24.04.1 LTS, Kernel 6.8.0-147, 20 vCPU                                                                                                                                                                                                                      | `uname -a`, `/etc/os-release`                                |
| Docker         | Docker 27.3.1, Compose v2.29.7                                                                                                                                                                                                                                               | `docker --version`, `docker compose version`                 |
| RAM            | 62 GiB total, 36 GiB belegt, 26 GiB verfügbar; Swap 10/31 GiB belegt                                                                                                                                                                                                         | `free -h`                                                    |
| Disk           | `/` = `/dev/md2`, 904 G, 610 G belegt, 249 G frei (72 %)                                                                                                                                                                                                                     | `df -h`                                                      |
| Edge           | Caddy läuft als Container `mellon-caddy`, `NetworkMode=host`, Ports 80/443, TLS via ACME **DNS-01** (Cloudflare)                                                                                                                                                             | `docker ps`, `docker inspect mellon-caddy`, Caddyfile-Header |
| Edge-Start     | Via systemd-Unit `caddy-planet.service` → `/opt/scripts/caddy-planet-run.sh`; Caddyfile per Bind-Mount `/home/momo/Caddyfile`                                                                                                                                                | `systemctl cat caddy-planet.service`                         |
| Edge-Caddyfile | Handgepflegt, ~200 Zeilen, viele `.bak`-Dateien im Home; **kein** `import`                                                                                                                                                                                                   | Auszug Caddyfile, `ls /home/momo/Caddyfile*`                 |
| Caddy-Admin    | 2019 auf dem Host belegt (Caddy-Default-Admin; Edge-Container läuft mit `NetworkMode=host`)                                                                                                                                                                                  | `ss -lnt`                                                    |
| Ports belegt   | 8000–8496 dicht belegt (Radio-Bestandssystem), 8888, 8989, 9000, 9080, 9090, 9093, 9696, 9707–9712, 18789, 18793, 18794, 25565–25580, 32400                                                                                                                                  | `ss -lnt`                                                    |
| Ports frei     | 12000–12099, 12100–18788, 19000–25560                                                                                                                                                                                                                                        | Ableitung aus `ss -lnt`                                      |
| Firewall       | `ufw` aktiv; Regeln nur 22/tcp, 51820/udp, 8085/tcp. Veröffentlichte Docker-Ports sind trotzdem erreichbar (Docker umgeht ufw-INPUT)                                                                                                                                         | `ufw status`, Beobachtung 25565/19132                        |
| Monitoring     | `mellon-prometheus` (`mellon-network`), `mellon-alertmanager`, `mellon-grafana`, `mellon-node-exporter`, `mellon-cadvisor`, `mellon-blackbox-exporter`; Prometheus-Config statisch gemountet aus `/opt/apps/monitoring/prometheus/prometheus.yml` (kein `file_sd` im Auszug) | `docker inspect`, `grep scrape_configs`                      |
| Logs           | `promtail` (Netz `plg-stack_default`) mountet `/var/lib/docker/containers`, `/var/run/docker.sock`, `/var/log/journal`                                                                                                                                                       | `docker inspect promtail`                                    |
| Backups        | `restic` auf dem Host, Repo `rclone:sbox01:/home/Backups/restic-planet`, Passwortdatei `/root/.restic-pw`, daily `restic-backup.timer`, weekly `restic-prune.timer`                                                                                                          | `/opt/scripts/restic-backup.sh`, `systemctl list-timers`     |
| Deploy-User    | `deploy` (uid 1002) existiert, keine zusätzlichen Gruppen                                                                                                                                                                                                                    | `id deploy`                                                  |
| Runner         | Self-hosted GitHub Runner unter `/opt/actions-runner` (Repo `momokli/openclaw-deploy`, Name `projectmellon`) + weitere                                                                                                                                                       | `/opt/actions-runner/.runner`                                |
| Netzwerke      | u. a. `mellon-network`, `bridge`, viele projektspezifische                                                                                                                                                                                                                   | `docker network ls`                                          |

**Beobachtungen außerhalb des Scopes (nicht geändert, nur gemeldet):**

- `/opt/scripts/caddy-planet-run.sh` enthält den Cloudflare-API-Token **im
  Klartext** (Wert hier bewusst nicht wiedergegeben). Empfehlung an den
  Host-Owner: Token rotieren und aus einer 0600-Env-Datei laden.
- `restic-backup.timer` hat keinen nächsten Lauf (`next elapse: -`) und lief
  zuletzt am 2026-10-25 — Ursache ungeprüft.
- Dieses Repo hat auf `v2` noch keinen Commit; es gibt folglich noch keine
  CI-Trigger und keine Image-Tags (§2.8).

### 1.4 Abhängigkeiten (nicht auflösbar in diesem Dokument)

- **Audience-/Live-Contract** des Schedulers (Deploy-Gate, §2.9) muss in
  `data-model.md`/`portal.md` festgelegt werden.
- **HLS-Details** (Segmentlänge, Renditions, ob überhaupt zum Start) kommen aus
  `liquidsoap.md`.
- **Auth-Modus** des Portals: `data-model.md` §2.7/§3 legt Login gegen
  `POST /internal/auth/verify` und opake Session-Tokens in `portal.db` fest;
  `portal.md` bestätigt bzw. ergänzt das (kein OIDC vorgesehen).

## 2. Entscheidungen

Format: **Entscheidung → Begründung → verworfene Alternative.**

### 2.1 Topologie: ein Host, zwei Compose-Projekte, ein geteilter Edge

- **Entscheidung:** DEV und PROD laufen auf demselben Host (`planet`) als zwei
  getrennte Compose-Projekte `zukka-dev` und `zukka-prod` mit je eigenem
  Port-Block, eigenen Volumes, eigenen Netzwerken und eigener Env-Datei. Der
  öffentliche 443-Listener bleibt beim bestehenden Shared Edge; der Stack-Caddy
  terminiert in PROD **kein** TLS, sondern routet intern.
- **Begründung:** Der Host ist die einzige verfügbare Maschine; der 443-Port ist
  durch den Edge-Caddy belegt. Ein zweiter Edge pro Host wäre ein
  Blast-Radius-Risiko für alle anderen Domains.
- **Verworfen:** eigener 443-Listener im Stack (Kollision); PROD auf separater
  Maschine (nicht vorhanden).

### 2.2 Ports: reservierter Block 12000–12099 (prod) / 12100–12199 (dev)

- **Entscheidung:** Loopback-Bindings für alles Interne, ein einziges
  öffentliches Roh-TCP für Caster-Ingest.

| Dienst                                 | PROD (Host-Bind)  | DEV (Host-Bind)                                                      |
| -------------------------------------- | ----------------- | -------------------------------------------------------------------- |
| icecast HTTP                           | `127.0.0.1:12000` | `127.0.0.1:12100`                                                    |
| liquidsoap Harbor                      | `0.0.0.0:12001`   | `127.0.0.1:12101`                                                    |
| scheduler API                          | `127.0.0.1:12010` | `127.0.0.1:12110`                                                    |
| portal (a/b)                           | kein Host-Port    | kein Host-Port                                                       |
| tapes                                  | kein Host-Port    | kein Host-Port                                                       |
| Caddy (App-Router)                     | `127.0.0.1:12080` | `0.0.0.0:12180`                                                      |
| Caddy Health                           | intern `:12081`   | intern `:12181`                                                      |
| Caddy (TLS, nur `ZUKKA_EDGE_MODE=own`) | `0.0.0.0:12443`   | `0.0.0.0:12444` (getrennt, damit DEV und PROD parallel testbar sind) |
| Metrics (scheduler)                    | `127.0.0.1:12091` | `127.0.0.1:12191`                                                    |

- **Begründung:** 8000–8496 und 18789–18794 sind belegt (Fakt, §1.3). 12xxx ist
  frei und kollidiert nicht mit dem Bestandssystem. DEV belegt nur Loopback,
  damit ein DEV-Fehler nie öffentlich sichtbar wird.
- **Verworfen:** „nächster freier Port" dynamisch (nicht reproduzierbar,
  Firewall-Doku unmöglich).
- **Hinweis:** `ufw` schützt veröffentlichte Docker-Ports nicht (Fakt: Docker
  setzt eigene `DOCKER`-Ketten vor `INPUT`). Für 12001 (Harbor) ist deshalb eine
  explizite `DOCKER-USER`-Regel nötig, wenn Ingest nicht weltweit offen sein soll
  (§4, Frage 3).

### 2.3 Netzwerke: drei Netze pro Projekt, Ingest entkoppelt

- **Entscheidung:**
  - `zukka-<env>-front`: caddy ↔ portal, tapes, icecast (nur HTTP/Stream).
  - `zukka-<env>-back` mit `internal: true`: scheduler ↔ liquidsoap ↔ portal ↔ icecast (kein Internet-Egress).
  - `zukka-<env>-egress`: nur `scheduler` (music-api, Telegram), `portal`
    (music-api Upload) und `tapes` (music-api Byte-Range-Proxy laut `tapes.md`
    D9); `caddy` nur wenn `ZUKKA_EDGE_MODE=own` (ACME).
  - `liquidsoap` hat **keinen** Internet-Egress: es liest Tracks aus dem
    von `scheduler` befüllten `tracks_cache`-Volume.
- **Begründung:** Die Audio-Engine hat keine plausible Ursache für ausgehenden
  Internetverkehr; ein kompromittierter Liquidsoap-Prozess soll nicht exfiltrieren
  können. Zugleich bleibt der Scheduler die einzige Datei-Beschaffungsstelle
  Richtung music-api (deckt sich mit „Music API = Datei-Autorität").
- **Verworfen:** ein flaches Netz (unnötige Ost-West-Rechte).
- **Kopplung:** Der „scheduler schreibt Dateien, liquidsoap liest" genannte
  Pfad ist ein Interface zur `liquidsoap.md`-Doku (§4, Frage 7).

### 2.4 State: vier echte Datenquellen, alles andere ist Cache

- **Entscheidung:** Persistente Volumes (Backup-pflichtig): `scheduler_data`
  (SQLite), `portal_data` (SQLite, Sessions — laut `data-model.md` §2.7),
  `tapes_data` (System of Record für Tape-Metadaten + Waveform, laut `tapes.md`
  D3), `caddy_data` (nur DEV, interne CA).
  Alles andere ist **Cache oder Staging** mit harter Retention:
  - `tracks_cache` (von scheduler befüllt, LRU, Max-Größe konfigurierbar, verlustfrei reproduzierbar)
  - `recordings_staging` (Live-Mitschnitte bis Upload zu music-api; nach
    erfolgreichem Upload gelöscht; Notbremse: Löschung nach 14 Tagen mit Alarm)
  - `hls` (Segmente, jederzeit regenerierbar)
  - `tapes_media_cache` (gepufferte music-api-Audioantworten für den
    Range-Proxy, `tapes.md` D9; jederzeit regenerierbar)
- **Begründung:** Maximal kleiner Restore-Umfang, keine zweite Track-Autorität.
- **Verworfen:** Recording-Archiv lokal (widerspricht „Music API ist die einzige
  Datei-Autorität"); `tapes_data` als Cache zu behandeln (widerspricht `tapes.md` D3).

### 2.5 Caddy: zwei Modi, eine gemeinsame Routen-Datei

- **Entscheidung:** Die Routenlogik liegt einmal in
  `deploy/caddy/routes.caddy` (per `import` eingebunden) und wird von zwei
  kleinen Wrappern parametrisiert:
  - `Caddyfile.dev`: eigener Listener `0.0.0.0:12180`, TLS `tls internal`,
    Domains `radio.dev`, `cast.dev`, `portal.dev`, `tapes.dev`.
  - `Caddyfile.prod`: `auto_https off`, Listener `127.0.0.1:12080`, Match auf
    `Host`-Header der PROD-Domains; TLS macht der Shared Edge.
  - `ZUKKA_EDGE_MODE=own` (PROD-Parallelbetrieb vor dem Cutover): eigener
    TLS-Listener `0.0.0.0:12443` mit ACME DNS-01 (Cloudflare), damit der Stack
    **ohne jede Änderung am Shared Edge** öffentlich testbar ist.
- **Begründung:** Kein Duplikat der Routing-Regeln; der Cutover auf 443 ist eine
  ~10-zeilige, repo-verwaltete Snippet-Datei plus eine einmalige `import`-Zeile.
- **Verworfen:** Routen direkt in `/home/momo/Caddyfile` pflegen (handgepflegte
  Datei mit vielen Backups, Fehler treffen alle Domains des Hosts).
- **Wichtig (Fakt):** `.dev` ist HSTS-preloaded — Browser akzeptieren dort
  **keine** selbstsignierten Zertifikate ohne manuellen Override. Für DEV heißt
  das: entweder echte Zertifikate via DNS-01 (Variante `*.dev.zukkafabrik.de`)
  oder bewusst `tls internal` mit Browser-Ausnahme (§4, Frage 1).

### 2.6 Secrets: Host ist die Quelle der Wahrheit, CI sieht nur den Deploy-Key

- **Entscheidung:** Env-Dateien mit Secrets liegen ausschließlich auf dem Host
  (`/etc/zukka/{dev,prod}.env`, `0600 root:zukka`), werden nie in CI gerendert
  und nie als Artefakt hochgeladen. `env/*.env.example` enthält nur Schlüssel,
  Defaults und `<secret>`-Platzhalter. Rotation = Datei ändern +
  `systemctl restart zukka-<env>` (bzw. Einzel-Service-Neustart, §2.9).
- **Begründung:** Kein Secret-Sync-Pfad, der brechen kann; kein GitHub-Secret,
  das eine ganze Umgebung auf einmal autorisiert.
- **Verworfen:** Secrets als GitHub-Environment-Secrets → env-Rendering beim
  Deploy (vervielfacht Secret-Kopien und macht den Host-Deployzustand von CI
  abhängig).
- **Ehrlichkeits-Hinweis:** Wer `docker`-Zugriff auf dem Host hat, kann
  Container-Envs lesen. Das ist bei `env_file` unvermeidbar; für kritische
  Secrets (z. B. `MUSIC_API_TOKEN`) ist der Docker-`secrets:`-Mount (Datei)
  vorzuziehen, sobald die Services ihn unterstützen.

### 2.7 Prozessaufsicht: systemd pro Umgebung, Compose nur als Executor

- **Entscheidung:** Je Umgebung eine `Type=oneshot`-Unit mit
  `RemainAfterExit=yes`, die `docker compose up -d --wait --remove-orphans`
  ausführt; `ExecStop` fährt sauber herunter. Zusätzlich Timer-Units für Backup
  und Restore-Drill.
- **Begründung:** Der Host nutzt dieses Muster bereits (`caddy-planet.service`);
  `--wait` erzwingt „healthy vor Boot-Abschluss", und `journalctl -u zukka-prod`
  ist eine einheitliche Log-Quelle. `restart: unless-stopped` in Compose deckt
  Container-Ebene, systemd die Boot-Reihenfolge.
- **Verworfen:** nur `restart: always` ohne systemd (keine Boot-Ordnung, kein
  `--wait`, keine einheitliche Unit für Deploy/Status).

### 2.8 CI/CD: GitHub Actions; Deploy über SSH-Wrapper, nicht über den Host-Runner

- **Entscheidung:** Drei Workflows (§3.5): `ci`, `deploy-dev`, `deploy-prod`.
  Images landen in GHCR (`ghcr.io/<owner>/zukka-{scheduler,portal,tapes,liquidsoap,icecast}:<sha>`).
  Deployment passiert per SSH auf `zukka-deploy@cast.zukkafabrik.de`, das nur
  den root-eigenen Wrapper `/usr/local/bin/zukka-deploy` ausführen darf
  (`NOPASSWD`, exakter Pfad + Argument-Whitelist). GitHub Environments `dev`
  und `prod`; `prod` mit Required Reviewer.
- **Begründung:** Der Host hat bereits Self-hosted Runner (`/opt/actions-runner*`,
  `.runner` nennt das Repo `momokli/openclaw-deploy`); ob deren User Docker-Zugriff
  haben, ist **nicht verifiziert**. Ein Runner auf einer Maschine voller fremder
  Stacks soll aber nicht implizit Prod-Rechte bekommen: der Wrapper ist der
  einzige Deploy-Pfad und validiert Umgebung + Tag.
- **Verworfen:** Deploy-Job auf dem vorhandenen Host-Runner (Runner-Kompromiss =
  Prod-Kompromiss, keine Audit-Grenze).
- **Offen (Fakt):** Repo hat auf `v2` noch keinen Commit; Image-Namen/Owner
  (`ghcr.io/<owner>`) sind Platzhalter bis zum ersten Push.

### 2.9 Deployment-Strategie

**Kernprinzip:** Die Hörbarkeit des Deploys hängt allein daran, ob `liquidsoap`
oder `icecast` neu startet. Alles andere ist unkritisch, solange der
Audio-Pfad weiterläuft.

| Dienst               | Zustand                                                                          | Strategie                                                                                     | Hörbare Unterbrechung                                            |
| -------------------- | -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| `portal`             | einziger Writer auf `portal.db` (Sessions, `data-model.md` §2.7) → **1 Instanz** | Recreate (`up -d --no-deps --wait`); Sessions überleben im DB-Volume                          | Portal-UI 1–3 s nicht erreichbar; Audio unberührt                |
| `tapes`              | einziger Writer auf `tapes.db` (System of Record) → **1 Instanz**                | Recreate; laufende Range-Downloads brechen ab, Clients retryen                                | kein Audio-Ausfall, einzelne Downloads müssen neu starten        |
| `scheduler`          | SQLite-Single-Writer                                                             | Recreate im ruhigen Fenster; Migrationen expand/contract                                      | keine (Audio läuft aus Liquidsoap-Queue weiter), API 10–30 s weg |
| `caddy` (App-Router) | stateless                                                                        | `caddy validate` + `caddy reload` (kein Container-Neustart)                                   | keine                                                            |
| `liquidsoap`         | stateful (Live-Handoffs, Crossfades)                                             | **Nur** im leeren Fenster oder mit `--force`; max. 1×/Tag                                     | 2–5 s Stille + Crossfade-Verlust                                 |
| `icecast`            | stateful (alle Listener)                                                         | Nur im leeren Fenster oder mit `--force`; Config-Änderung möglichst per Reload statt Recreate | Verbindungsabbruch aller Hörer (~5–15 s bis Reconnect)           |

**Gate „niemand hört" (PROD):** Der Deploy-Job fragt vor jedem Recreate von
`scheduler`, `liquidsoap` oder `icecast` den Scheduler:

```
GET /internal/ops/audience   (Service-Token)
→ { "listeners": 0, "live_sources": 0, "next_broadcast_start": "2026-11-19T20:00:00+01:00" }
```

Regeln:

1. `listeners == 0 && live_sources == 0 && next_broadcast_start > now + 15 min`
   → sofort deployen.
2. sonst → warten bis Fenster **03:00–04:00 Europe/Berlin** (DST-korrekt über
   `TZ`, nie über UTC-Offsets).
3. Operator-Override nur über `workflow_dispatch`-Input `force=true` und mit
   Pflicht-Bestätigung im Environment `prod`; Vorwarnung via Telegram 2 min vorher.

**Reihenfolge eines PROD-Deploys:**
`predeploy-backup` → `pull` → `scheduler` (Gate) → `portal` →
`tapes` → `caddy reload` → `liquidsoap` (nur bei Bedarf/Gate) → `icecast` (nur
bei Bedarf/Gate) → Smoke-Tests → `.deployed`-State schreiben.

**Rollback:** `/opt/apps/radio-zukka/prod/.deployed` enthält `sha`, `image_tag`,
`timestamp`, `ref`. `zukka-deploy prod --rollback` setzt `ZUKKA_TAG` auf den
vorherigen Tag und fährt denselben Ablauf. Automatischer Rollback, wenn
Smoke-Tests fehlschlagen. Voraussetzung: Migrationen expand/contract (alte
Binary muss mit neuem Schema starten).

### 2.10 Backup/Restore: eigener restic-Repo-Pfad, DB-konsistent, Drill getestet

- **Entscheidung:** Eigener restic-Repo-Pfad auf derselben StorageBox
  (`rclone:sbox01:/home/Backups/restic-planet-zukka`), eigene Passwortdatei
  `/etc/zukka/restic.pw` (`0600 root`). Backups immer **DB-konsistent** über
  `sqlite3 <db> ".backup"` in ein Temp-Verzeichnis, dann restic. Zusätzlich
  Pre-Deploy-Snapshot (nur DBs). Wöchentlicher Restore-Drill als Timer.
- **Begründung:** Isolierter Repo-Pfad trennt Retention/Prune und verhindert,
  dass ein `forget --prune` der allgemeinen Sicherung die Radio-Historie kürzt.
- **Verworfen:** zweiter Backup-Mechanismus (borg/rsync) neben dem Host-standard
  restic.

### 2.11 Monitoring: bestehende Prometheus/Alertmanager/Grafana/Loki-Kette nutzen

- **Entscheidung:** Kein eigener Prometheus. Der Stack liefert Metriken, die in
  den bestehenden `mellon-prometheus` aufgenommen werden (einmalige
  Shared-Änderung: ein Scrape-Job), Alerts als eigener Regelblock, Grafana
  bekommt ein Dashboard-JSON aus dem Repo.
- **Begründung:** Der Host hat bereits Prometheus, Alertmanager inkl.
  Telegram-Bridge, Grafana, Loki und blackbox-exporter (Fakt, §1.3) — eine
  zweite Kette wäre Doppelarbeit und mehr RAM-Druck.
- **Verworfen:** eigenes Monitoring-Bundle im Stack.

### 2.12 Images: eigene, digest-gepinnte Images; nur Liquidsoap aus upstream

- **Entscheidung:** `scheduler`, `portal`, `tapes` als Rust-Binaries auf
  `gcr.io/distroless/cc-debian12` (kein Shell, Healthcheck über
  `--healthcheck`-Subkommando). `icecast` als eigenes schlankes Image
  (`debian:bookworm-slim` + gepinnte Distribution-Version). `liquidsoap` aus dem
  offiziellen `savonet/liquidsoap`-Image, **per Digest** gepinnt, nicht per Tag.
- **Begründung:** Kein unkontrolliertes `:latest` (der Edge-Caddy nutzt
  `:latest` — genau das soll der neue Stack nicht), reproduzierbare Builds,
  minimale Angriffsfläche.
- **Verworfen:** Icecast aus einer fremden Community-Image-Quelle ohne
  Digest-Pin.

### 2.13 Logging: Docker `json-file` mit Rotation (nicht journald)

- **Entscheidung:** `logging: json-file` mit `max-size: 20m`, `max-file: 5`.
  Kein journald-Treiber.
- **Begründung:** `promtail` mountet `/var/lib/docker/containers` (Fakt) und
  scrapt damit Docker-JSON-Logs. journald-Treiber würde für promtail unsichtbar
  sein. _Inferenz_, nicht durch die promtail-Config verifiziert (§4, Frage 11).
- **Verworfen:** journald (bricht die Log-Kette, ohne Gewinn).

## 3. Schnittstellen (Env, Compose, Caddy, CI)

### 3.1 Env-Schema

Zwei Dateien im Repo (`env/dev.env.example`, `env/prod.env.example`) mit
identischem Schlüsselsatz; nur Defaults unterscheiden sich. Jede Variable hat
eine Zeile Kommentar. `<secret>` = darf nie in Git; wird auf dem Host in
`/etc/zukka/<env>.env` gesetzt.

Gruppen: **Core**, **Domains/Edge**, **Ports**, **Icecast**, **Harbor**,
**Scheduler**, **Portal**, **Tapes**, **Music API**, **Telegram**, **Storage**,
**Backup**, **Observability**, **Images**.

#### `env/dev.env.example`

```dotenv
# ── Core ───────────────────────────────────────────────────────────────
ZUKKA_ENV=dev
ZUKKA_TZ=Europe/Berlin
ZUKKA_TAG=dev
ZUKKA_LOG_LEVEL=debug
RUST_LOG=info,zukka=debug

# ── Domains / Edge ─────────────────────────────────────────────────────
# HSTS-Vorbelegung von .dev beachten: nur mit echten Zertifikaten nutzbar.
ZUKKA_DOMAIN_RADIO=radio.dev
ZUKKA_DOMAIN_CAST=cast.dev
ZUKKA_DOMAIN_PORTAL=portal.dev
ZUKKA_DOMAIN_TAPES=tapes.dev
# own = eigener TLS-Listener (12443), shared = Plain-HTTP hinter Shared Edge
ZUKKA_EDGE_MODE=own
ZUKKA_ACME_EMAIL=<secret>            # Kontaktadresse für ACME (DNS-01)
CLOUDFLARE_API_TOKEN=<secret>        # nur bei ZUKKA_EDGE_MODE=own
ZUKKA_EDGE_HOST_PORT=12444

# ── Ports (Host-Bind, DEV-Block) ───────────────────────────────────────
ZUKKA_PORT_ICECAST=12100
ZUKKA_PORT_HARBOR=12101
ZUKKA_PORT_SCHEDULER=12110
ZUKKA_PORT_CADDY=12180
ZUKKA_PORT_METRICS=12191

# ── Icecast ────────────────────────────────────────────────────────────
# Die icecast-Config wird beim Container-Start aus einem Template gerendert
# (#109, envsubst); diese Secrets braucht sie. SOURCE muss zu Liquidsoap passen.
ZUKKA_ICECAST_SOURCE_PASSWORD=<secret>
ZUKKA_ICECAST_RELAY_PASSWORD=<secret>
ZUKKA_ICECAST_ADMIN_PASSWORD=<secret>
ZUKKA_ICECAST_ADMIN_USER=admin          # optional, Default admin
ICECAST_HOSTNAME=cast.dev
ICECAST_PORT=8000
ICECAST_MAX_LISTENERS=200
ICECAST_LOG_LEVEL=3

# ── Harbor (Caster-Ingest) ─────────────────────────────────────────────
HARBOR_PORT=8001
HARBOR_INTERNAL_PASSWORD=<secret>     # nur interner Mount für Jingles/Fallback
HARBOR_MAX_CONNECTIONS=10
HARBOR_TIMEOUT_SECONDS=30
HARBOR_TLS=false                      # true erst, wenn §4/3 geklärt ist

# ── Scheduler ──────────────────────────────────────────────────────────
SCHEDULER_DB=/data/scheduler.db
SCHEDULER_TICK_MS=500
SCHEDULER_LIVE_POLL_MS=1000
SCHEDULER_QUEUE_MIN_FILL_SECONDS=30
SCHEDULER_TRACKS_CACHE_DIR=/cache/tracks
SCHEDULER_TRACKS_CACHE_MAX_GB=20
SCHEDULER_HLS_DIR=/hls                # read-only Mount, nur für Health-Age
SCHEDULER_SERVICE_TOKEN=<secret>      # `Authorization: Bearer` für portal → scheduler (data-model §3)
LIQUIDSOAP_TOKEN=<secret>             # `X-Liquidsoap-Token` für liquidsoap → scheduler (data-model §3)

# ── Portal ─────────────────────────────────────────────────────────────
# Auth: Login gegen `POST /internal/auth/verify` des Schedulers, Sessions als
# opake Tokens (SHA-256-Hash) in `portal.db` — kein App-Secret nötig (data-model §2.7/§3).
PORTAL_DB=/data/portal.db
PORTAL_SECURE_COOKIES=true            # Cookie: HttpOnly, Secure, SameSite=Lax
PORTAL_UPLOAD_MAX_BYTES=2147483648

# ── Tapes ──────────────────────────────────────────────────────────────
# tapes_data ist System of Record für Tape-Metadaten + Waveform (tapes.md D3);
# Audio-Bytes bleiben bei music-api und werden per Proxy gestreamt (tapes.md D9).
TAPES_DB=/data/tapes.db
TAPES_MEDIA_CACHE_DIR=/cache/media
TAPES_MEDIA_CACHE_MAX_GB=50
TAPES_FEED_PAGE_SIZE=50

# ── Music API (externe Autorität) ──────────────────────────────────────
MUSIC_API_BASE_URL=http://127.0.0.1:5001
MUSIC_API_TOKEN=<secret>
MUSIC_API_TIMEOUT_SECONDS=20

# ── Telegram (Announcements, Ops-Alarme) ───────────────────────────────
TELEGRAM_BOT_TOKEN=<secret>
TELEGRAM_CHANNEL_ID=<secret>
TELEGRAM_OPS_CHANNEL_ID=<secret>
# Hinweis: Der Scheduler liest fuer Announcements den eigenen Namensraum
# `ZUKKA_TELEGRAM_BOT_TOKEN`, `ZUKKA_TELEGRAM_CHAT_ID` (sowie in DEV
# `ZUKKA_TELEGRAM_CHAT_ID_TEST` und `ZUKKA_ANNOUNCEMENT_POLL_SECS`). Unset →
# Versand deaktiviert. Die generischen Namen oben bleiben fuer Bridge/Ops.

# ── Storage ────────────────────────────────────────────────────────────
ZUKKA_HLS_SEGMENT_SECONDS=4
ZUKKA_RECORDINGS_STAGING_MAX_DAYS=14

# ── Backup (nur für systemd-Timer, nicht in den Containern) ────────────
RESTIC_REPOSITORY=rclone:sbox01:/home/Backups/restic-planet-zukka
RESTIC_PASSWORD_FILE=/etc/zukka/restic.pw

# ── Observability ──────────────────────────────────────────────────────
ZUKKA_METRICS_ENABLED=true
ZUKKA_METRICS_ADDR=0.0.0.0:9091

# ── Images ─────────────────────────────────────────────────────────────
ZUKKA_IMAGE_REGISTRY=ghcr.io/<owner>
ZUKKA_IMAGE_LIQUIDSOAP=savonet/liquidsoap@sha256:<digest>
```

#### `env/prod.env.example`

Gleicher Schlüsselsatz; abweichende Werte:

```dotenv
ZUKKA_ENV=prod
ZUKKA_TAG=<sha>                       # vom Deploy gesetzt, nie manuell
ZUKKA_LOG_LEVEL=info
RUST_LOG=info

# Domains: vorläufig 12443-Parallelbetrieb, Cutover siehe §6 Phase 6
ZUKKA_DOMAIN_RADIO=radio.zukkafabrik.de
ZUKKA_DOMAIN_CAST=ingest.zukkafabrik.de    # cast.* bleibt bis Abschaltung belegt (§4/2)
ZUKKA_DOMAIN_PORTAL=portal.zukkafabrik.de
ZUKKA_DOMAIN_TAPES=tapes.zukkafabrik.de
ZUKKA_EDGE_MODE=own                   # Phase 6: auf shared umstellen
ZUKKA_ACME_EMAIL=<secret>
CLOUDFLARE_API_TOKEN=<secret>
ZUKKA_EDGE_HOST_PORT=12443

ZUKKA_PORT_ICECAST=12000
ZUKKA_PORT_HARBOR=12001
ZUKKA_PORT_SCHEDULER=12010
ZUKKA_PORT_CADDY=12080
ZUKKA_PORT_METRICS=12091

ICECAST_HOSTNAME=radio.zukkafabrik.de
ICECAST_MAX_LISTENERS=1000
ICECAST_LOG_LEVEL=2

HARBOR_MAX_CONNECTIONS=20
SCHEDULER_TRACKS_CACHE_MAX_GB=60
TAPES_MEDIA_CACHE_MAX_GB=150

RESTIC_REPOSITORY=rclone:sbox01:/home/Backups/restic-planet-zukka
RESTIC_PASSWORD_FILE=/etc/zukka/restic.pw

ZUKKA_METRICS_ADDR=0.0.0.0:9091
ZUKKA_IMAGE_REGISTRY=ghcr.io/<owner>
ZUKKA_IMAGE_LIQUIDSOAP=savonet/liquidsoap@sha256:<digest>
```

**Secret-Liste (vollständig):** `CLOUDFLARE_API_TOKEN`, `ZUKKA_ICECAST_SOURCE_PASSWORD`,
`ZUKKA_ICECAST_RELAY_PASSWORD`, `ZUKKA_ICECAST_ADMIN_PASSWORD`, `HARBOR_INTERNAL_PASSWORD`,
`SCHEDULER_SERVICE_TOKEN`,
`LIQUIDSOAP_TOKEN`, `MUSIC_API_TOKEN`, `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHANNEL_ID`,
`TELEGRAM_OPS_CHANNEL_ID`, `ZUKKA_ACME_EMAIL`, plus host-seitig `RESTIC_PASSWORD`
(in `restic.pw`) und `GHCR_READ_TOKEN` (nur Host-Docker-Login, nie App-Env).

**Regeln:** `env/*.env` ist gitignored (`.gitignore` deckt `.env`, aber nicht
`env/*.env` — deshalb zusätzlich `env/*.env` eintragen). CI prüft per
`gitleaks`, dass kein Secret committet wird.

### 3.2 Docker Compose

Dateien: `deploy/compose/compose.base.yml` (Anker/Common),
`compose.dev.yml`, `compose.prod.yml` (shared-Basis) und `compose.prod-own.yml`
(Override nur `ZUKKA_EDGE_MODE=own`, Issue #91). Aufruf immer mit
`-p zukka-<env> --env-file /etc/zukka/<env>.env`.

**Verzeichnis-Layout auf dem Host** (alle relativen Bind-Mounts der Compose-Dateien
beziehen sich auf das Verzeichnis der Compose-Datei):

```text
/opt/apps/radio-zukka/
├── repo/          # git checkout (deployt aus CI) — compose/, caddy/, scripts/, systemd/, env/*.example
├── dev/           # DEV-Status: .deployed, .deployed.prev, Log-Symlinks
└── prod/          # PROD-Status: .deployed, .deployed.prev, Log-Symlinks
/etc/zukka/
├── dev.env        # 0600 root:zukka   (Secrets, §2.6)
├── prod.env       # 0600 root:zukka
├── backup.env     # 0600 root:root    (RESTIC_*)
└── restic.pw      # 0600 root:root
```

Service-Matrix:

| Service      | Image                                                   | Innen-Port        | Volumes                                                    | Netz(e)                                             |
| ------------ | ------------------------------------------------------- | ----------------- | ---------------------------------------------------------- | --------------------------------------------------- |
| `icecast`    | `…/zukka-icecast:$TAG`                                  | 8000              | `icecast_logs`, `hls` (r/w für Fallback-Dateien optional)  | front, back                                         |
| `liquidsoap` | `savonet/liquidsoap@sha256:…`                           | 8001 (Harbor)     | `tracks_cache` (ro), `hls` (rw), `recordings_staging` (rw) | back, front                                         |
| `scheduler`  | `…/zukka-scheduler:$TAG`                                | 8080, 9091        | `scheduler_data`, `tracks_cache` (rw), `hls` (ro)          | back, egress                                        |
| `portal`     | `…/zukka-portal:$TAG`                                   | 8080              | `portal_data` (rw)                                         | front, back, egress                                 |
| `tapes`      | `…/zukka-tapes:$TAG`                                    | 8080              | `tapes_data` (rw), `tapes_media_cache` (rw)                | front, egress (music-api-Proxy)                     |
| `caddy`      | `caddy:2.9` bzw. `ghcr.io/caddybuilds/caddy-cloudflare` | 12080/12081/12091 | `hls` (ro), `caddy_data`, `repo/caddy/*` (ro)              | front, ggf. egress, ggf. `mellon-network` (Metrics) |

Skelett `compose.prod.yml` (`compose.dev.yml` erbt und überschreibt Ports/Replicas):

```yaml
name: zukka-prod

x-logging: &logging
  driver: json-file
  options: { max-size: "20m", max-file: "5" }

x-defaults: &defaults
  restart: unless-stopped
  logging: *logging
  security_opt: ["no-new-privileges:true"]
  env_file: ["/etc/zukka/prod.env"]

networks:
  front: {}
  back: { internal: true }
  egress: {}
  mellon-network: { external: true } # nur caddy, für Prometheus-Scrape

volumes:
  hls: {}
  tracks_cache: {}
  recordings_staging: {}
  scheduler_data: {}
  portal_data: {}
  tapes_data: {}
  tapes_media_cache: {}
  caddy_data: {}
  icecast_logs: {}

services:
  icecast:
    <<: *defaults
    image: ${ZUKKA_IMAGE_REGISTRY}/zukka-icecast:${ZUKKA_TAG}
    ports: ["127.0.0.1:${ZUKKA_PORT_ICECAST}:8000"]
    volumes:
      - icecast_logs:/var/log/icecast
      - hls:/srv/hls
    networks: [front, back]
    healthcheck:
      test: ["CMD-SHELL", "curl -fsS http://127.0.0.1:8000/status-json.xsl >/dev/null"]
      interval: 10s
      timeout: 3s
      retries: 5
    deploy: { resources: { limits: { memory: 512M } } }

  liquidsoap:
    <<: *defaults
    image: ${ZUKKA_IMAGE_LIQUIDSOAP}
    command: ["/etc/liquidsoap/radio.liq"]
    ports: ["0.0.0.0:${ZUKKA_PORT_HARBOR}:8001"]
    volumes:
      - ../liquidsoap/radio.liq:/etc/liquidsoap/radio.liq:ro
      - tracks_cache:/cache/tracks:ro
      - hls:/srv/hls
      - recordings_staging:/recordings
    networks: [back, front]
    depends_on:
      icecast: { condition: service_healthy }
    healthcheck:
      test:
        [
          "CMD-SHELL",
          "test $$(( $$(date +%s) - $$(stat -c %Y /srv/hls/index.m3u8 || echo 0) )) -lt 15",
        ]
      interval: 10s
      timeout: 3s
      retries: 3
      start_period: 60s
    deploy: { resources: { limits: { memory: 1G, cpus: "2" } } }

  scheduler:
    <<: *defaults
    image: ${ZUKKA_IMAGE_REGISTRY}/zukka-scheduler:${ZUKKA_TAG}
    ports:
      - "127.0.0.1:${ZUKKA_PORT_SCHEDULER}:8080"
      - "127.0.0.1:${ZUKKA_PORT_METRICS}:9091" # lokale Diagnose; Scrape läuft über caddy:12091
    volumes:
      - scheduler_data:/data
      - tracks_cache:/cache/tracks
      - hls:/hls:ro
    networks: [back, egress]
    healthcheck:
      test: ["CMD", "/usr/local/bin/scheduler", "--healthcheck"]
      interval: 10s
      timeout: 5s
      retries: 5
      start_period: 30s
    deploy: { resources: { limits: { memory: 512M } } }

  portal:
    <<: *defaults
    image: ${ZUKKA_IMAGE_REGISTRY}/zukka-portal:${ZUKKA_TAG}
    volumes: ["portal_data:/data"]
    networks: [front, back, egress]
    healthcheck:
      test: ["CMD", "/usr/local/bin/portal", "--healthcheck"]
      interval: 10s
      timeout: 5s
      retries: 5
    deploy: { resources: { limits: { memory: 512M } } }

  tapes:
    <<: *defaults
    image: ${ZUKKA_IMAGE_REGISTRY}/zukka-tapes:${ZUKKA_TAG}
    volumes:
      - "tapes_data:/data"
      - "tapes_media_cache:/cache/media"
    networks: [front, egress] # music-api-Byte-Range-Proxy (tapes.md D9)
    healthcheck:
      test: ["CMD", "/usr/local/bin/tapes", "--healthcheck"]
      interval: 15s
      timeout: 5s
      retries: 5
    deploy: { resources: { limits: { memory: 256M } } }

  caddy:
    <<: *defaults
    image: caddy:2.9 # shared-Basis; own-Override: cloudflare-Image (unten)
    # shared = Caddyfile.prod, nur 127.0.0.1:12080 (kein TLS, kein 12443).
    # own = via compose/prod-own.yml (Caddyfile.prod-own, 0.0.0.0:12443).
    command:
      [
        "caddy",
        "run",
        "--config",
        "/etc/caddy-config/Caddyfile.prod",
        "--adapter",
        "caddyfile",
      ]
    ports:
      - "127.0.0.1:${ZUKKA_PORT_CADDY}:12080"
    volumes:
      - ../caddy:/etc/caddy-config:ro
      - hls:/srv/hls:ro
      - caddy_data:/data
    networks: [front, egress, mellon-network]
    depends_on:
      portal: { condition: service_healthy }
    healthcheck:
      test: ["CMD-SHELL", "wget -qO- http://127.0.0.1:12081/health >/dev/null"]
      interval: 10s
      timeout: 3s
      retries: 5
    deploy: { resources: { limits: { memory: 256M } } }
```

Override `compose.prod-own.yml` (nur `ZUKKA_EDGE_MODE=own`; wird zusammen mit
`compose.prod.yml` aufgerufen — Compose kann Ports/command nicht per Env-Wert
umschalten, daher das `-f`-Override):

```yaml
# docker compose -f compose/prod.yml -f compose/prod-own.yml --env-file env/prod.env up -d
services:
  caddy:
    image: ghcr.io/caddybuilds/caddy-cloudflare:2.9 # Pin per Digest im Betrieb
    command:
      [
        "caddy",
        "run",
        "--config",
        "/etc/caddy-config/Caddyfile.prod-own",
        "--adapter",
        "caddyfile",
      ]
    ports:
      # Compose konkateniert `ports`; die Basis-12080 bleibt, hier nur 12443:
      - "0.0.0.0:${ZUKKA_EDGE_HOST_PORT}:12443"
```

Anmerkungen, die beim Implementieren beachtet werden müssen:

- `env_file` zeigt bewusst auf `/etc/zukka/<env>.env`; im Repo liegt nur
  `env/<env>.env.example`. Für lokale Läufe: `--env-file env/dev.env`.
- Der Healthcheck von `liquidsoap` misst die **Frische der HLS-Playlist**
  (mtime) — das ist das ehrlichste Lebenszeichen der Audio-Kette. Startet der
  Stack ohne HLS (`liquidsoap.md`-Entscheidung offen), ist stattdessen ein
  `icecast`-Metadaten-Check (`/admin/metadata?mount=…&mode=up`) nötig
  (§4, Frage 8).
- `security_opt: no-new-privileges` gilt für alle; `liquidsoap` braucht kein
  `pid: host`, keinen Docker-Socket, keine Geräte.
- Alle Ports außer Harbor und (im own-Modus) 12443 sind Loopback.
- `compose.dev.yml` überschreibt den Caddy-Publish auf `0.0.0.0:${ZUKKA_PORT_CADDY}`,
  damit DEV aus LAN/Tailscale erreichbar ist (§2.2); PROD bleibt auf `127.0.0.1`.

### 3.3 Caddy

Dateien:

- `deploy/caddy/routes.caddy` — gemeinsame Routen, per Env-Platzhaltern
  parametrisiert.
- `deploy/caddy/Caddyfile.dev` — Listener 12180, `tls internal`, importiert `routes.caddy`.
- `deploy/caddy/Caddyfile.prod` — `auto_https off`, Listener 12080 (Ziel des
  Shared Edge); `auto_https off` gilt, TLS macht der Edge.
- `deploy/caddy/Caddyfile.prod-own` — Parallelbetrieb vor dem Cutover: gleiche
  Routen, Site `https://:12443` mit
  `tls { dns cloudflare {env.CLOUDFLARE_API_TOKEN} }` **plus** interner
  `http://:12080`-Router (analog `Caddyfile.prod`, damit der Smoke-Gate
  `scripts/smoke.sh prod` greift); `admin off` + `auto_https off` (Datei wird
  per Override `compose/prod-own.yml` gewählt, nicht per Caddy-Bedingung).
- `deploy/caddy/edge/zukka-prod.caddy` — Snippet für den Shared Edge (Phase 6).

`routes.caddy` (Kern; Werte aus Env):

```caddyfile
# gemeinsame Routen — erwartet: {env.ZUKKA_DOMAIN_*} gesetzt
(radio_zukka_routes) {
	# ── radio.<dom>: Player, Stream, HLS ───────────────────────────
	@radio host {$ZUKKA_DOMAIN_RADIO}
	handle @radio {
		# HLS: Playlist nie cachen, Segmente kurz und immutable
		handle_path /hls/* {
			root * /srv/hls
			@playlist path *.m3u8
			header @playlist Cache-Control "no-store"
			@segment path *.ts *.m4s *.mp4
			header @segment Cache-Control "public, max-age=30, immutable"
			header @segment Content-Type video/mp2t
			header Access-Control-Allow-Origin "*"
			file_server
		}

		# Live-Stream: ungepuffert, kein Encoding
		handle /stream.mp3* {
			reverse_proxy icecast:8000 {
				flush_interval -1
				transport http {
					response_header_timeout 0
				}
			}
			header Cache-Control "no-store"
		}

		# Statische Player-Seite
		handle {
			root * /srv/www/radio
			encode zstd gzip
			try_files {path} /index.html
			file_server
		}
	}

	# ── cast.<dom>: Caster-Ingest & Anleitung ──────────────────────
	@cast host {$ZUKKA_DOMAIN_CAST}
	handle @cast {
		# HTTP-PUT-Ingest auf Harbor (Primärweg bleibt Roh-TCP 12001)
		handle /live* {
			reverse_proxy liquidsoap:8001 {
				flush_interval -1
			}
		}
		handle {
			root * /srv/www/cast
			encode zstd gzip
			file_server
		}
	}

	# ── portal.<dom>: Control Plane ────────────────────────────────
	@portal host {$ZUKKA_DOMAIN_PORTAL}
	handle @portal {
		header {
			Strict-Transport-Security "max-age=31536000; includeSubDomains"
			X-Content-Type-Options "nosniff"
			Referrer-Policy "strict-origin-when-cross-origin"
			X-Frame-Options "DENY"
			-Server
		}
		request_body {
			max_size 2GB
		}
		encode zstd gzip
		reverse_proxy portal:8080 {
			lb_try_duration 5s
			health_uri /healthz
			health_interval 5s
			health_timeout 2s
			fail_duration 15s
		}
	}

	# ── tapes.<dom>: öffentlicher Feed ─────────────────────────────
	@tapes host {$ZUKKA_DOMAIN_TAPES}
	handle @tapes {
		encode zstd gzip
		reverse_proxy tapes:8080 {
			lb_try_duration 5s
			health_uri /healthz
			health_interval 5s
			fail_duration 15s
		}
	}
}
```

`Caddyfile.prod` (Auszug):

```caddyfile
{
	admin off
	auto_https off
	servers {
		trusted_proxies static 127.0.0.1/32
	}
}

# Interner App-Router (Ziel des Shared Edge)
http://:12080 {
	import radio_zukka_routes
	# Diagnose nur intern
	handle /__zu/health { respond "ok" 200 }
}

# Health-Endpoint für den Container-Healthcheck
http://:12081 {
	respond /health "ok" 200
	respond 404
}

# Metrics-Brücke für den geteilten Prometheus (Netz: mellon-network)
http://:12091 {
	reverse_proxy scheduler:9091
}
```

`Caddyfile.prod-own` (Auszug, nur vor dem Cutover):

```caddyfile
{
	admin off
	auto_https off
	servers {
		trusted_proxies static 127.0.0.1/32
	}
}

# Interner App-Router (Ziel des Smoke-Gates), analog Caddyfile.prod.
http://:12080 {
	handle /__zu/health { respond "ok" 200 }
	import radio_zukka_routes
}

# Öffentlicher eigener TLS-Listener (own mode, ACME DNS-01).
https://:12443 {
	tls {
		dns cloudflare {env.CLOUDFLARE_API_TOKEN}
	}
	import radio_zukka_routes
}
```

`Caddyfile.dev` (Auszug):

```caddyfile
{
	admin 0.0.0.0:2019
	local_certs
}

https://:12180 {
	tls internal
	import radio_zukka_routes
}
```

**Hinweise:**

- `.dev` + HSTS: `tls internal` funktioniert technisch, aber Browser blocken es
  ohne Override. Siehe §4, Frage 1.
- `flush_interval -1` ist die zentrale Einstellung gegen Delay; `encode` wird
  auf Audio nicht angewendet (nur kompressible Typen), trotzdem ist `encode`
  nicht in den Stream-Blöcken aktiviert.
- Der Caddy-Container bekommt in PROD `tls` gar nicht zu sehen; dort terminiert
  der Edge. Das HSTS-Header aus `routes.caddy` ist dann trotzdem korrekt, weil
  der Edge die Verbindung mit TLS betreibt.
- Reload statt Restart: `docker compose exec -T caddy caddy reload --config /etc/caddy/Caddyfile.prod --adapter caddyfile`.
- Der Health-Site-Block (`http://:12081`) und die Metrics-Brücke (`http://:12091`)
  müssen in **allen** drei Caddyfiles stehen; in DEV mit den Ports `:12181`/`:12191`
  und angepasstem Compose-Healthcheck.

**Shared-Edge-Snippet** `deploy/caddy/edge/zukka-prod.caddy` (Phase 6):

```caddyfile
# BEGIN RADIO ZUKKA (verwaltet von radio/zukka_radio, nicht manuell editieren)
radio.zukkafabrik.de, ingest.zukkafabrik.de, portal.zukkafabrik.de, tapes.zukkafabrik.de {
	encode zstd gzip
	request_body {
		max_size 2GB
	}
	reverse_proxy 127.0.0.1:12080 {
		flush_interval -1
	}
}
# END RADIO ZUKKA
```

Einbindung in den Edge (einmalig, manuell durch Host-Owner):

1. `mkdir -p /opt/apps/caddy/sites`
2. In `/opt/scripts/caddy-planet-run.sh` eine Zeile ergänzen:
   `-v /opt/apps/caddy/sites:/etc/caddy/sites:ro`
3. In `/home/momo/Caddyfile` **eine** Zeile ergänzen:
   `import /etc/caddy/sites/*.caddy`
4. Edge-Container neu erzeugen — nur im Fenster 03:00 (kurze Downtime für alle
   Domains des Hosts).

**Belegt (Doku caddyserver.com, `import`):** „It is an error if a specific file
cannot be found, but an empty glob pattern is not an error." → ein leerer
`sites`-Ordner bricht den Edge-Glob nicht.

### 3.4 systemd-Units

`deploy/systemd/zukka-prod.service`:

```ini
[Unit]
Description=radio zukka — PROD stack (compose project zukka-prod)
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
User=zukka
Group=docker
EnvironmentFile=/etc/zukka/prod.env
WorkingDirectory=/opt/apps/radio-zukka/repo/compose
ExecStart=/usr/bin/docker compose -p zukka-prod \
    --env-file /etc/zukka/prod.env \
    -f /opt/apps/radio-zukka/repo/compose/compose.base.yml \
    -f /opt/apps/radio-zukka/repo/compose/compose.prod.yml \
    up -d --wait --remove-orphans
ExecStop=/usr/bin/docker compose -p zukka-prod \
    --env-file /etc/zukka/prod.env \
    -f /opt/apps/radio-zukka/repo/compose/compose.base.yml \
    -f /opt/apps/radio-zukka/repo/compose/compose.prod.yml \
    stop --timeout 20
TimeoutStartSec=600
Restart=on-failure
RestartSec=30

[Install]
WantedBy=multi-user.target
```

`zukka-dev.service`: identisch mit `-p zukka-dev`, `EnvironmentFile=/etc/zukka/dev.env`,
`Restart=no` (DEV darf aus bleiben, ohne Alarme zu erzeugen).

`zukka-backup.service` + `zukka-backup.timer`:

```ini
[Unit]
Description=radio zukka — restic backup (DBs + Staging)

[Service]
Type=oneshot
User=root
EnvironmentFile=/etc/zukka/backup.env
ExecStart=/usr/local/bin/zukka-backup backup

[Install]
WantedBy=timers.target
```

```ini
[Unit]
Description=radio zukka — tägliches Backup (04:15 Berlin)

[Timer]
OnCalendar=*-*-* 04:15:00 Europe/Berlin
Persistent=true
RandomizedDelaySec=300

[Install]
WantedBy=timers.target
```

`zukka-restore-drill.service` + `.timer`: wöchentlich (Sonntag 05:30 Berlin)
`zukka-backup drill` → restauriert `scheduler.db` und `portal.db` aus dem
jüngsten Snapshot nach `/var/tmp/zukka-drill`, führt `PRAGMA integrity_check`
und einen Schema-Count aus, löscht danach; Fehlschlag → Exit ≠ 0 → Alertmanager
`NodeSystemd`-Regel bzw. Telegram (§3.9).

**Hinweis:** `User=zukka` + `Group=docker` ist ehrlich gesagt **keine**
Sicherheitsgrenze (Docker-Socket = root-äquivalent). Die Trennung dient der
Nachvollziehbarkeit der Prozesse, nicht der Isolation.

### 3.5 CI/CD (GitHub Actions)

`.github/workflows/ci.yml`

```yaml
name: ci
on:
  pull_request:
  push: { branches: [v2] }
jobs:
  test:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace --all-features
      - run: cargo sqlx migrate info --source scheduler/migrations # nur wenn sqlx genutzt
  lint-infra:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - run: docker run --rm -v "$PWD/deploy/caddy:/cfg" caddy:2.9 caddy validate --config /cfg/Caddyfile.prod --adapter caddyfile
      - run: docker run --rm -v "$PWD/deploy/caddy:/cfg" caddy:2.9 caddy validate --config /cfg/Caddyfile.dev  --adapter caddyfile
      - run: docker compose -f deploy/compose/compose.base.yml -f deploy/compose/compose.prod.yml --env-file env/prod.env.example config -q
      - run: docker compose -f deploy/compose/compose.base.yml -f deploy/compose/compose.dev.yml  --env-file env/dev.env.example  config -q
      - run: shellcheck scripts/*.sh
      - uses: gitleaks/gitleaks-action@v2
  build:
    needs: [test, lint-infra]
    if: github.event_name == 'push'
    runs-on: ubuntu-24.04
    permissions: { contents: read, packages: write }
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          {
            registry: "ghcr.io",
            username: "${{ github.actor }}",
            password: "${{ secrets.GITHUB_TOKEN }}",
          }
      - run: ./scripts/build-images.sh "${{ github.sha }}"
```

`.github/workflows/deploy-dev.yml` (Push auf `v2` + CI grün):

```yaml
name: deploy-dev
on:
  workflow_run: { workflows: [ci], types: [completed], branches: [v2] }
concurrency: { group: deploy-dev, cancel-in-progress: true }
jobs:
  deploy:
    if: github.event.workflow_run.conclusion == 'success'
    runs-on: ubuntu-24.04
    environment: dev
    steps:
      - name: Deploy
        run: |
          ssh -i ~/.ssh/deploy -o StrictHostKeyChecking=yes zukka-deploy@cast.zukkafabrik.de \
            "sudo /usr/local/bin/zukka-deploy dev --tag ${{ github.event.workflow_run.head_sha }}"
      - name: Smoke
        run: ./scripts/smoke.sh dev
```

`.github/workflows/deploy-prod.yml` (Tag `v*` oder manuell):

```yaml
name: deploy-prod
on:
  push: { tags: ["v*"] }
  workflow_dispatch:
    inputs:
      force:
        description: "Fenster-Gate übergehen (nur mit Ansage)"
        type: boolean
        default: false
      tag:
        description: "Image-Tag/SHA (leer = aktueller main-Stand des Tags)"
        type: string
concurrency: { group: deploy-prod, cancel-in-progress: false }
jobs:
  gate:
    runs-on: ubuntu-24.04
    environment: prod # Required Reviewer
    outputs: { tag: "${{ steps.t.outputs.tag }}" }
    steps:
      - id: t
        run: echo "tag=${{ inputs.tag || github.sha }}" >> "$GITHUB_OUTPUT"
      - name: Audience-Gate
        run: |
          ssh -i ~/.ssh/deploy zukka-deploy@cast.zukkafabrik.de \
            "sudo /usr/local/bin/zukka-deploy prod --check-window ${{ inputs.force && '--force' || '' }}"
  deploy:
    needs: gate
    runs-on: ubuntu-24.04
    environment: prod
    steps:
      - run: |
          ssh -i ~/.ssh/deploy zukka-deploy@cast.zukkafabrik.de \
            "sudo /usr/local/bin/zukka-deploy prod --tag ${{ needs.gate.outputs.tag }}"
      - run: ./scripts/smoke.sh prod
```

**Wrapper** `/usr/local/bin/zukka-deploy` (root:root, 0755, `deploy/systemd/zukka-deploy`):

- `--tag <sha>`: prüft `^[0-9a-f]{7,40}$`, holt Images, schreibt `.deployed.next`,
  fährt die Reihenfolge aus §2.9 ab, Smoke-Gate durch Compose-`--wait`,
  committet `.deployed.next` → `.deployed`.
- `--check-window [--force]`: fragt `GET /internal/ops/audience` ab, entscheidet
  sofort / Fenster / Abbruch, Exit-Code 0 = darf deployen, 3 = Gate zu.
- `--rollback`: liest `.deployed.prev` und deployt mit dem alten Tag.
- `sudoers`-Eintrag: `zukka-deploy ALL=(root) NOPASSWD: /usr/local/bin/zukka-deploy`
  (exakter Pfad, Argumente werden im Skript validiert).
- `docker login ghcr.io` auf dem Host mit `GHCR_READ_TOKEN` (read:packages) ist
  einmalig manuell einzurichten.

**Secrets/Environments:** GitHub Environment `dev` (keine Reviewer) und `prod`
(Required Reviewer, Deployment-Branches nur Tags). Ablage: `DEPLOY_SSH_KEY` als
Environment-Secret, `DEPLOY_HOST`, `DEPLOY_USER`. Host-Secrets liegen **nicht**
in GitHub (§2.6).

### 3.6 Healthcheck-Verträge (Implementierungsauflage)

| Endpoint                     | Dienst                   | Semantik                                                       |
| ---------------------------- | ------------------------ | -------------------------------------------------------------- |
| `GET /healthz`               | portal, tapes, scheduler | Prozess lebt (kein I/O außer Heap-Check), 200                  |
| `GET /readyz`                | scheduler, portal        | DB schreibbar (Test-Transaktion/`PRAGMA quick_check`), 200/503 |
| `GET /internal/ops/audience` | scheduler                | `{listeners, live_sources, next_broadcast_start}` (Gate, §2.9) |
| `GET /metrics`               | scheduler                | Prometheus-Format                                              |
| `/status-json.xsl`           | icecast                  | Upstream-Feature, von scheduler für Listener-Zahlen gepollt    |
| `--healthcheck` (CLI)        | scheduler/portal/tapes   | Exit 0/1, distroless-tauglich                                  |

Der Scheduler ist die **einzige** Stelle, die Hörerzahlen interpretiert
(icecast-Polling), damit Gate, Announcements und Metriken dieselbe Quelle haben.

`/internal/ops/audience` ist eine **Anforderung dieses Plans** und in
`data-model.md` §3 noch zu ergänzen (§4, Frage 5). `GET /metrics` und die
`--healthcheck`-Flags sind ebenfalls dort bzw. in `portal.md`/`tapes.md` zu
verankern.

### 3.7 Backup/Restore-Kommandos

```bash
# Backup (systemd: zukka-backup.timer, 04:15 Berlin)
zukka-backup backup                 # DB-Snapshots + Staging + Env-Dateien
zukka-backup predeploy <sha>        # nur DBs, Tag "predeploy-<sha>" (Deploy-Wrapper)

# Retention: 14 daily, 8 weekly, 6 monthly; Prune sonntags 05:00 Berlin
restic --repo "$RESTIC_REPOSITORY" forget --keep-daily 14 --keep-weekly 8 \
       --keep-monthly 6 --tag zukka --prune

# Konvention: `zukka-backup` legt pro Lauf einen Staging-Baum
# /var/tmp/zukka-backup-stage/<env>/ an und sichert daraus RELATIVE Pfade
# (data/scheduler.db, data/portal.db, env/*.env, deploy.sha, …).

# Restore (Notfall)
systemctl stop zukka-prod
restic restore latest --tag zukka --target /var/tmp/restore-zukka
install -o zukka -g zukka -m 0600 /var/tmp/restore-zukka/data/scheduler.db \
        /var/lib/docker/volumes/zukka-prod_scheduler_data/_data/scheduler.db
install -o zukka -g zukka -m 0600 /var/tmp/restore-zukka/data/portal.db \
        /var/lib/docker/volumes/zukka-prod_portal_data/_data/portal.db
systemctl start zukka-prod
curl -fsS http://127.0.0.1:12010/readyz

# Restore-Drill (wöchentlich, automatisiert)
zukka-backup drill                  # integrity_check + Schema-Vergleich, Exit ≠ 0 → Alert
```

Backup-Inhalt: `scheduler.db`, `portal.db`, `tapes.db` (+ Waveforms), `recordings_staging` (nur
„Upload ausstehend"), `/etc/zukka/*.env`, `caddy_data` (DEV-CA),
`deploy/`-Stand (mittels `git rev-parse` + `sha` in `.deployed`).
**Nicht** gesichert: `hls`, `tracks_cache`, `tapes_media_cache` (regenerierbar),
Icecast-Logs.

Ziele: **RPO ≤ 24 h** (plus Pre-Deploy-Snapshot), **RTO ≤ 30 min** für den
Audio-Betrieb (scheduler/portal-DB-Restore).

### 3.8 Monitoring & Alarme

Der Stack liefert Metriken auf `:9091`, erreichbar über die Caddy-Brücke
`:12091` im Netz `mellon-network`; `zukka-prod-caddy` ist dafür als einziges
Container im geteilten Netz (§3.2).

Metriken (Minimum):

| Metrik                            | Quelle                      | Zweck                                                    |
| --------------------------------- | --------------------------- | -------------------------------------------------------- |
| `zukka_icecast_listeners`         | scheduler (Poll)            | Publikum, Deploy-Gate, Alarme                            |
| `zukka_live_sources`              | scheduler                   | Live-Status                                              |
| `zukka_source_state{layer}`       | scheduler                   | welche Ebene spielt (live/show/queue/rotation/relay/fun) |
| `zukka_hls_segment_age_seconds`   | scheduler (read-only Mount) | Audio-Kette lebt                                         |
| `zukka_queue_fill_seconds{queue}` | scheduler                   | Unterlauf-Gefahr                                         |
| `zukka_music_api_errors_total`    | scheduler/portal            | externe Autorität erreichbar                             |
| `zukka_scheduler_db_locked_total` | scheduler                   | SQLite-Contention                                        |
| `zukka_build_info{version,sha}`   | alle                        | Rollback-Nachweis                                        |

Alarme (Block `radio_zukka` in `/opt/apps/monitoring/prometheus/rules.yml`):

| Alarm                         | Bedingung (Skizze)                                                    | Schwere             |
| ----------------------------- | --------------------------------------------------------------------- | ------------------- |
| `ZukkaAudioDown`              | `zukka_hls_segment_age_seconds > 20` für 1 m                          | critical (Telegram) |
| `ZukkaIcecastDown`            | `up{job="zukka-*"} == 0` für 2 m                                      | critical            |
| `ZukkaNoListenersInBroadcast` | `listeners == 0` während geplanter Sendung > 5 m                      | warning             |
| `ZukkaQueueUnderrunRisk`      | `zukka_queue_fill_seconds < 30` für 2 m                               | warning             |
| `ZukkaDiskPressure`           | node-exporter: `use% > 85` auf `/`                                    | warning             |
| `ZukkaBackupFailed`           | `node_systemd_unit_state{name="zukka-backup.service",state="failed"}` | critical            |
| `ZukkaCertExpiry`             | blackbox `probe_ssl_earliest_cert_expiry - time() < 14d`              | warning             |
| `ZukkaRestartLoop`            | `changes(container_start_time_seconds[15m]) > 2` (cadvisor)           | warning             |

Blackbox-Ziele zusätzlich aufnehmen: `https://radio.<dom>/hls/index.m3u8`,
`https://portal.<dom>/healthz`, `https://tapes.<dom>/healthz`.

**Einmalige Shared-Änderungen** (Host-Owner, im Fenster):

1. `prometheus.yml`: neuer Job `zukka-prod` mit Ziel
   `zukka-prod-caddy:12091` (Netz `mellon-network` ist vorhanden — Fakt).
2. `rules.yml`: Regelblock aus dem Repo einbinden (Repo → Host per Deploy).
3. Blackbox-Ziele ergänzen.
4. Grafana-Dashboard-JSON aus `deploy/monitoring/dashboard.json` importieren.
5. Prometheus-Reload-Mechanismus verifizieren (`/-/reload` nur mit
   `--web.enable-lifecycle`; sonst `kill -HUP`) — siehe §4, Frage 6.

## 4. Offene Fragen

| #   | Frage                                                                                                                                                                                                                                                                                                                                                   | Blockiert                          | Klärung durch               |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- | --------------------------- |
| 1   | Sind `radio.dev`, `cast.dev`, `portal.dev`, `tapes.dev` echte öffentliche Namen im Eigentum des Projekts? `.dev` ist HSTS-preloaded → selbstsignierte Certs sind im Browser wertlos. Variante A: `*.dev.zukkafabrik.de` mit Cloudflare DNS-01. Variante B: `tls internal` + Browser-Override (schlechte Gewohnheit).                                    | DEV-TLS, Caddyfile.dev             | Projekt-Owner               |
| 2   | Bleiben die PROD-Namen unter `zukkafabrik.de`? `cast.zukkafabrik.de` ist aktuell an 127.0.0.1:8075 gebunden (Bestandssystem) → bis zu dessen Abschaltung nicht verfügbar. Vorschlag: `ingest.zukkafabrik.de` als Caster-Ingest, `cast.*` erst nach Cutover.                                                                                             | PROD-Domains, Edge-Snippet         | Projekt-Owner               |
| 3   | Harbor-Ingest: rohes TCP 12001 (Klartext-Credentials im Internet) oder TLS? Ob `input.harbor` TLS terminiert, ist **nicht verifiziert** (Doku-Seite lieferte nur das Referenz-Index; muss am gepinnten Image geprüft werden: `liquidsoap --list-plugins` / `liquidsoap -h input.harbor`). Ohne TLS bleibt nur Roh-TCP + starke Passwörter + Rate-Limit. | `HARBOR_TLS`, Firewall, Sicherheit | Liquidsoap-Verantwortliche  |
| 4   | `ufw`/`DOCKER-USER`: Soll 12001 nur für bekannte Caster-IPs offen sein, oder weltweit? Aktuell schützt ufw Docker-Ports nicht (Fakt).                                                                                                                                                                                                                   | Firewall-Regeln                    | Host-Owner                  |
| 5   | Scheduler-Endpunkt `/internal/ops/audience` — in `data-model.md` §3 noch **nicht** enthalten; muss dort ergänzt werden (Felder insb. `next_broadcast_start`, Timezone-Format).                                                                                                                                                                          | Deploy-Gate, CI                    | `data-model.md`             |
| 6   | Prometheus-Reload: erlaubt `mellon-prometheus` `/-/reload`? Ist die Config ansible-verwaltet (dann muss der Job dorthin, nicht in die Datei)?                                                                                                                                                                                                           | Monitoring-Anbindung               | Host-Owner                  |
| 7   | Beschafft der Scheduler Track-Dateien und legt sie in `tracks_cache` (Annahme hier), oder zieht Liquidsoap selbst per HTTP von music-api? Beeinflusst Netzwerke/Egress.                                                                                                                                                                                 | Compose-Netze, Liquidsoap          | `liquidsoap.md`             |
| 8   | HLS: welche Segmentlänge/Renditions, ist es zum Start aktiv? Bestimmt den Liquidsoap-Healthcheck und das Blackbox-Ziel.                                                                                                                                                                                                                                 | Healthchecks, Monitoring           | `liquidsoap.md`             |
| 9   | Braucht das Portal je zwei Instanzen (Zero-Downtime-UI)? `data-model.md` §2.7 legt Sessions in `portal.db` + einen Writer pro DB fest → aktuell **eine** Instanz (§2.9). Zwei Instanzen erst mit externem Session-Store.                                                                                                                                | Rolling Restart                    | `portal.md`/`data-model.md` |
| 10  | Bleibt DEV dauerhaft auf `planet` (RAM-Druck, 26 GiB verfügbar; 36 GiB belegt) oder auf eine separate Maschine?                                                                                                                                                                                                                                         | Ressourcen-Limits                  | Projekt-Owner               |
| 11  | Scrapt `promtail` tatsächlich Docker-JSON-Logs (Mounts legen das nahe, Config nicht eingesehen)? Falls nein, ist das Logging-Ziel (§2.13) zu ändern.                                                                                                                                                                                                    | Logging                            | Host-Owner                  |
| 12  | Freigabe der Fenster-Policy: 03:00–04:00 Berlin für stateful Restarts, Override nur mit Reviewer?                                                                                                                                                                                                                                                       | Deploy-Policy                      | Projekt-Owner               |
| 13  | Token-Trennung ist in `data-model.md` §3 belegt: `SCHEDULER_SERVICE_TOKEN` (Bearer, portal→scheduler) und `LIQUIDSOAP_TOKEN` (`X-Liquidsoap-Token`). Offen bleibt nur die **Rotation**: Wie oft und mit welchem Übergangsfenster (zwei gültige Tokens parallel)?                                                                                        | Secret-Rotation                    | Projekt-Owner               |
| 14  | Icecast-Config-Reload statt Recreate möglich (`/admin/...` mit `reload`-Semantik in der genutzten Version)? Nicht verifiziert.                                                                                                                                                                                                                          | Icecast-Deploys                    | Umsetzung Phase 3           |

## 5. Risiken

| #   | Risiko                                                                                              | Wirkung                                             | Mitigation                                                                                                                    | Restrisiko                     |
| --- | --------------------------------------------------------------------------------------------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------ |
| R1  | DEV und PROD auf **einem** Host (Fakt: 36/62 GiB RAM belegt, Swap 10 GiB belegt)                    | OOM/Kernel-Problem stoppt Radio                     | Harte `memory`-Limits pro Container (§3.2), DEV nicht autostart, DEV-Block nur Loopback                                       | mittel — bleibt Einzelpunkt    |
| R2  | Edge-Caddyfile ist handgepflegt (viele `.bak`, kein `import`, `:latest`-Image)                      | Ein Fehler trifft **alle** Domains des Hosts        | Repo-Snippet + `import`-Glob (leerer Glob ist erlaubt — belegt), `caddy validate` vor `reload`, Änderung nur im 03:00-Fenster | mittel                         |
| R3  | Edge-Deploy ist nicht in unserer CI (fremder systemd-Unit/Skript)                                   | Drift zwischen Repo-Snippet und Realität            | Snippet im Repo als Quelle, Phase-6-Schritt als Checkliste, Diff-Abgleich im Deploy-Skript                                    | niedrig                        |
| R4  | Cloudflare-Token im Klartext in `/opt/scripts/caddy-planet-run.sh` (Fakt, Wert nicht wiedergegeben) | Host-Kompromiss → DNS/ACME-Übernahme                | Meldung an Host-Owner, Rotation, Auslagerung in 0600-Datei; unser `CLOUDFLARE_API_TOKEN` nur in `/etc/zukka/<env>.env`        | mittel (fremde Datei)          |
| R5  | Ein einziger Harbor-Rohport ohne TLS (falls §4/3 nicht klärbar)                                     | Caster-Credentials im Klartext                      | Starke Passwörter, kurze Rotation, IP-Allowlist, TLS-Terminator als Folgeausbau                                               | hoch, bis TLS steht            |
| R6  | Portal und Tapes sind Single-Writer (SQLite) → kein echtes Rolling-Restart                          | 1–3 s UI-/Download-Lücke beim Deploy                | Recreate nur im ruhigen Fenster; Audio-Pfad bleibt unberührt; 2 Instanzen erst mit geklärtem Session-Store (data-model §2.7)  | niedrig                        |
| R7  | Portal/Tapes sind Single-Instance → beim Recreate keine Ausweichinstanz                             | Caster-UI bzw. Downloads 1–3 s weg                  | Kurzer `stop_grace_period`, Recreate im ruhigen Fenster, Audio-Pfad unberührt                                                 | niedrig                        |
| R8  | Disk `/` 72 % voll (249 G frei); Caches/Staging wachsen                                             | Vollaufen → SQLite-Schreibfehler, Audio-Abbruch     | `tracks_cache` Max-GB, Staging-Löschung nach 14 Tagen + Alarm, Disk-Alert bei 85 %                                            | niedrig–mittel                 |
| R9  | Pre-Deploy-Snapshot schlägt fehl, Deploy läuft trotzdem                                             | Kein Rollback-Punkt                                 | Wrapper bricht bei Backup-Fehler ab (Exit ≠ 0), CI zeigt Fehler                                                               | niedrig                        |
| R10 | Migrationen nicht abwärtskompatibel                                                                 | Rollback auf alte Binary scheitert                  | Expand/Contract als Auflage, Migrationstest in CI (`migrate info` + Rollback-Test in DEV)                                     | niedrig                        |
| R11 | Liquidsoap/icecast Recreate während Sendung                                                         | Hörer weg, Crossfade verloren                       | Audience-Gate, Fenster, `--force` nur mit Ansage                                                                              | mittel (bewusst)               |
| R12 | `restic-backup.timer` des Hosts lief seit 2026-10-25 nicht (Fakt, Ursache unklar)                   | Auch unser Backup könnte unbemerkt ausfallen        | Eigenes Unit/Timer + `ZukkaBackupFailed`-Alert + wöchentlicher Drill                                                          | niedrig                        |
| R13 | GHCR-Pull auf dem Host braucht Token                                                                | Deploy bricht bei Token-Ablauf                      | `GHCR_READ_TOKEN` (read:packages), Ablaufdatum dokumentiert, Smoke-Test meldet Pull-Fehler                                    | niedrig                        |
| R14 | Self-hosted Runner auf dem Host vorhanden (Fakt); Docker-Zugriff des Runner-Users nicht geprüft     | Runner-Kompromiss ≈ Host-Kompromiss                 | Unser Deploy läuft **nicht** über diesen Runner (SSH-Wrapper, §2.8)                                                           | gering für uns, offen für Host |
| R15 | Repo ohne Commit auf `v2` (Fakt)                                                                    | Keine Tags, keine Image-Namen, CI-Trigger ins Leere | Phase 2 legt Initial-Commit + Image-Namen `<owner>` fest, `deploy-prod` startet erst nach erstem `v*`-Tag                     | niedrig                        |
| R16 | HLS-Healthcheck über Datei-mtime kann bei NFS/Timing falsch positiv sein                            | Restart-Loop oder falsch grüner Status              | 3× Segmentdauer als Schwelle, `start_period 60s`, zusätzlich Icecast-Metadaten-Check (§4/8)                                   | niedrig                        |

## 6. Umsetzungsschritte

Reihenfolge ist verbindlich; jeder Schritt endet mit einer prüfbaren
Verifikation.

**Phase 0 — Vorprüfung (read-only, keine Änderung)**

1. `ss -lntp` erneut prüfen, dass 12000–12099/12100–12199 frei sind.
2. `liquidsoap -h input.harbor` im gepinnten Image → TLS-Support (§4/3).
3. `docker inspect mellon-prometheus --format '{{.HostConfig.NetworkMode}}'` + Config-Reload-Fähigkeit (§4/6).
4. `promtail`-Config sichten, ob Docker-JSON gescrapt wird (§4/11).
5. DNS-Zonen und Inhaberschaft der DEV/PROD-Namen klären (§4/1, §4/2).

**Phase 1 — Repo-Gerüst (keine Host-Änderung)**

6. `env/dev.env.example`, `env/prod.env.example` (§3.1); `.gitignore` um `env/*.env` erweitern.
7. `deploy/compose/{compose.base,compose.dev,compose.prod}.yml` (§3.2).
8. `deploy/caddy/{routes.caddy,Caddyfile.dev,Caddyfile.prod,Caddyfile.prod-own,edge/zukka-prod.caddy}` (§3.3).
9. `deploy/systemd/{zukka-dev.service,zukka-prod.service,zukka-backup.service,zukka-backup.timer,zukka-restore-drill.service,zukka-restore-drill.timer,zukka-deploy}` (§3.4, §3.5).
10. `scripts/{build-images.sh,smoke.sh,audience-gate.sh}`, `.github/workflows/{ci,deploy-dev,deploy-prod}.yml`, `.github/legacy-words.txt`.

_Verifikation:_ `caddy validate` für alle drei Caddyfiles; `docker compose config -q` für beide Envs; `shellcheck` grün.

**Phase 2 — Host-Bootstrap (einmalig, root, Fenster 03:00)**

11. Systemuser `zukka` (Gruppe `docker`), Layout nach §3.2 anlegen: `/opt/apps/radio-zukka/{repo,dev,prod}`, `/etc/zukka` (`0700 root:zukka`), `repo/` als git-Checkout.
12. `/etc/zukka/{dev,prod}.env` aus Example erzeugen, Secrets setzen, `0600 root:zukka`.
13. `/usr/local/bin/zukka-deploy` + `sudoers`-Regel für `zukka-deploy`.
14. `docker login ghcr.io` mit `GHCR_READ_TOKEN`.
15. `restic init` im Repo-Pfad `restic-planet-zukka`, `/etc/zukka/restic.pw` anlegen (`0600 root:root`).
16. Units installieren (`systemctl daemon-reload`), `zukka-dev` aktivieren, `zukka-prod` **nicht** aktivieren.

_Verifikation:_ `systemctl status zukka-dev` = `active`; `sudo -u zukka-deploy sudo -n /usr/local/bin/zukka-deploy --help` läuft; `restic snapshots` listet das leere Repo.

**Phase 3 — DEV-Bring-up**

17. Reihenfolge: `icecast` → `liquidsoap` (Stille/Testquelle) → `scheduler` → `portal` → `tapes` → `caddy`.
18. Smoke: `curl -fsS https://radio.dev:12180/hls/index.m3u8`, `curl -fsS https://radio.dev:12180/stream.mp3 -o /dev/null`, `curl -fsS https://portal.dev:12180/healthz` (jeweils mit `-k` bzw. importierter DEV-CA).
19. Listener-Test mit echtem Player (VLC/mpv) und einem Caster-Ingest gegen `<dev-host>:12101`.

_Verifikation:_ Audio hörbar, `/readyz` grün, `docker compose ps` zeigt alle `healthy`.

**Phase 4 — CI aktivieren**

20. Erster Commit auf `v2`, `ci` grün, Images in GHCR mit SHA-Tag.
21. `deploy-dev` einmal manuell auslösen; `scripts/smoke.sh dev` grün.

_Verifikation:_ `.deployed` enthält den SHA; `zukka_build_info{sha=…}` aller Services stimmt überein.

**Phase 5 — PROD-Parallelbetrieb (kein Cutover)**

22. `zukka-prod` mit `ZUKKA_EDGE_MODE=own` (Caddyfile.prod-own) starten: Domains `*.zukkafabrik.de`, Listener 12443.
23. Zwei Wochen Soak: Alarme beobachten, Hörerzahlen vergleichen, `restic`-Backups und Drill grün.
24. Deploy-Gate testen: künstlich `listeners > 0` erzeugen und prüfen, dass `--check-window` mit Exit 3 abbricht.

**Phase 6 — Cutover auf 443 (Fenster 03:00)**

25. Edge-Bootstrap: `sites`-Mount + `import`-Zeile + Edge-Recreate (§3.3) — **einziger** Eingriff am Shared Edge.
26. `deploy/caddy/edge/zukka-prod.caddy` nach `/opt/apps/caddy/sites/` deployen, `caddy validate`, `caddy reload`.
27. `ZUKKA_EDGE_MODE=shared` (Caddyfile.prod), `zukka-prod` neu erzeugen; `https://radio.zukkafabrik.de` liefert Audio.
28. Abschaltung des Bestandssystems (fremder Prozess): erst danach `cast.zukkafabrik.de` auf den neuen Stack umschreiben.

_Verifikation:_ `curl -I https://radio.zukkafabrik.de/stream.mp3` → 200; HLS-Playlist frisch; alle übrigen Domains des Hosts unverändert erreichbar.

**Phase 7 — Betrieb härten**

29. Prometheus-Job, Regelblock, Blackbox-Ziele, Grafana-Dashboard (§3.8).
30. Alarme durchspielen (HLS-Datei anhalten → `ZukkaAudioDown` innerhalb 2 min).
31. Restore-Drill real durchführen: `systemctl stop zukka-prod` → Restore → `readyz` → Hörer wieder da.
32. Rollback-Drill: `zukka-deploy prod --rollback` auf vorherigen SHA, Smoke grün.

**Phase 8 — Doku-Abschluss**

33. Dieses Dokument gegen die Realität abgleichen (Ports, Namen, Units).
34. Runbook-Zettel für: „Audio weg", „Backup rot", „Edge kaputt", „Deploy hängt".

## 7. Akzeptanzkriterien

**Repo/Gerüst**

1. `env/dev.env.example` und `env/prod.env.example` enthalten **denselben** Schlüsselsatz (per `diff <(cut -d= -f1 …)` geprüft) und jeder Schlüssel ist kommentiert.
2. Kein Secret liegt im Repo: `gitleaks detect` grün, `git grep -nE '(PASSWORD|TOKEN|SECRET)='` findet nur `<secret>`-Platzhalter.
3. `docker compose --env-file env/dev.env.example -f compose.base.yml -f compose.dev.yml config -q` und dieselbe Prüfung für PROD laufen fehlerfrei.
4. `caddy validate` ist für `Caddyfile.dev`, `Caddyfile.prod` und `Caddyfile.prod-own` grün.
5. `shellcheck scripts/*.sh` und die Units in `deploy/systemd/` bestehen `systemd-analyze verify`.

**Neue Welt**

6. Dieses Dokument nennt das Radio-Bestandssystem und den früheren Musikdienst nicht namentlich: `git grep -inFf .github/legacy-words.txt -- docs/` (Wortliste im CI, damit die Suchbegriffe selbst nicht im Dokument stehen) ist leer.
7. Alle Ports aus §2.2 kollidieren nicht mit den am 2026-11-18 belegten Ports.

**DEV**

8. Nach `systemctl start zukka-dev` sind alle Container `healthy` (max. 90 s).
9. `https://radio.dev:12180/stream.mp3` liefert einen hörbaren Stream; `/hls/index.m3u8` ist jünger als 3× Segmentdauer.
10. `portal.dev`, `tapes.dev`, `cast.dev` antworten mit 200 auf `/healthz` bzw. liefern ihre Seite.

**PROD**

11. `systemctl status zukka-prod` = `active (exited)`; `docker compose ps` zeigt alle `running (healthy)`.
12. `https://radio.zukkafabrik.de/stream.mp3` nach Cutover 200; Listener-Zahl in `zukka_icecast_listeners` sichtbar.
13. Portal-Recreate während Dauerlast (100 Requests in 10 s): höchstens 3 s ohne Antwort, **kein** Fehler auf dem Audio-Pfad (`/stream.mp3` läuft durch).
14. `icecast`/`liquidsoap`-Recreate ist ohne `--force` außerhalb des Fensters **nicht** möglich: `zukka-deploy prod --tag <sha>` bei `listeners > 0` endet mit Exit 3.
15. Neustart von `liquidsoap` im Fenster: Stille ≤ 5 s, danach wird `ZukkaAudioDown` nicht ausgelöst.

**Betrieb**

16. `zukka-backup backup` erzeugt einen Snapshot; `restic snapshots` zeigt ihn mit Tag `zukka` und `<sha>`; Drill ist grün.
17. Restore-Drill: Wiederherstellung von `scheduler.db` aus Snapshot in ≤ 30 min, `PRAGMA integrity_check` = `ok`.
18. Alarme greifen im Drill: `ZukkaAudioDown`, `ZukkaIcecastDown`, `ZukkaBackupFailed` jeweils ≤ 5 min nach Auslösung im Telegram-Ops-Channel.
19. `zukka_build_info{sha=…}` aller Services entspricht dem in `.deployed` vermerkten SHA.
20. Rollback auf den vorherigen SHA ist in ≤ 10 min durchgeführt und endet mit grünem Smoke-Test.
