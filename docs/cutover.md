# PROD-Cutover auf 443 (Issue #45)

Runbook für den einmaligen Cutover des PROD-Stacks von der Parallelbetriebs-
Stellung (`own`: eigener TLS-Listener `0.0.0.0:12443`) auf den Shared Edge
(443). Ab diesem Moment terminiert der Edge das TLS, der Stack hört nur noch
lokal auf `127.0.0.1:12080`.

Grundlage: `docs/consolidated-architecture.md` §5 (Source of Truth),
`docs/infrastructure.md` §2.5/§2.9/§3.3/§6 Phase 6/§7, `docs/deploy-prod.md`.

**Verantwortlich:** Projekt-Owner (Go/No-Go je Schritt) und Host-Owner
(Edge-Eingriff, DNS, Abschaltung des Bestandssystems). Ausführung nur im
Fenster **03:00–04:00 Europe/Berlin** (`infrastructure.md` §2.9, §6 Phase 6).
Zeiten sind immer Ortszeit `Europe/Berlin` — **nie** UTC-Offsets hartcodieren
(DST, `infrastructure.md` §2.9).

## 1. Abgrenzung

| Thema                                        | Issue | In diesem Runbook?                              |
| -------------------------------------------- | ----- | ----------------------------------------------- |
| Datenmigration (Tracks/Tapes/Caster)         | #46   | **nein** — eigenes Issue                        |
| Rollback-**Plan** (vollständig, Datenrückweg) | #47   | **nein** — eigenes Issue                        |
| Cutover (DNS, Edge, Compose/Env)             | #45   | **ja**                                          |
| Rollback-**Fenster** und -**Trigger**         | #45   | **ja** — operativ, ohne eigenen Rollback-Plan   |
| Deploy-/Rollback-Mechanik (`.deployed`)      | #41   | nur referenziert (`docs/deploy-prod.md`)        |

Dieses Runbook beschreibt **nur** den Cutover und das operative Rollback-
Fenster/-Trigger. Ein vollständiger Rollback-Plan inkl. Migrations-/Daten-
Rückweg ist #47 und wird hier nicht neu entworfen.

## 2. Vorbedingungen (Checkliste — vor dem Fenster abhaken)

Alle Punkte müssen **grün** sein, bevor das Fenster geöffnet wird. Ein roter
Punkt ist ein **Blocker vor dem Fenster**, nicht ein Fenster-Problem.

- [ ] **PROD-Parallelbetrieb ≥ 2 Wochen grün** (`infrastructure.md` §6 Phase 5,
      Schritt 23): Alarme beobachtet, Hörerzahlen verglichen.
- [ ] **`ZUKKA_EDGE_MODE=own` war real aktiv.** Konkret geprüft: der
      Caddy-Container wurde mit `Caddyfile.prod-own` gestartet und
      `0.0.0.0:12443` ist gemappt (`docker compose ps`, `ss -lnt`). Siehe
      Abschnitt 6 — im Repo ist dieser Modus **nicht** verdrahtet; ist er nicht
      nachweisbar, war Phase 5 nicht abgeschlossen und der Cutover darf nicht
      als Ersatz für den Soak-Test dienen.
- [ ] **`systemctl status zukka-prod` = `active (exited)`** und
      `docker compose ps` zeigt alle Container `running (healthy)`
      (`infrastructure.md` §7 Nr. 11).
- [ ] **Smoke grün:** `scripts/smoke.sh prod` (`docs/deploy-prod.md`).
- [ ] **Letztes Pre-Deploy-Backup grün:** `restic snapshots` zeigt einen
      Snapshot mit Tag `zukka` und `<sha>` (`docs/backup.md`).
- [ ] **Deploy-Mechanik verstanden:** `.deployed`/`.deployed.prev`-Rotation und
      `zukka-deploy prod --rollback` (`docs/deploy-prod.md`).
- [ ] **Host-Owner verfügbar** für Edge-Eingriff und DNS-Umschaltung
      (`/home/momo/Caddyfile`, `/opt/scripts/caddy-planet-run.sh`,
      Cloudflare-Zone) — fremder Verantwortungsbereich.
- [ ] **Fenster bestätigt** (03:00–04:00 Europe/Berlin) und die Downtime für
      **alle** Host-Domains angekündigt (Edge-Recreate, Abschnitt 5).
- [ ] **Alter DNS-Zielwert notiert** (Bestandssystem-IP) in der
      Ausführungs-Checkliste — der Rückweg braucht ihn (Abschnitt 9).

## 3. DNS-Umschaltung

Zone: Cloudflare für `zukkafabrik.de`. Umschaltung ist **manuell** (kein
Skript; siehe Abschnitt 9).

**Records und Zielwert:**

| Record                     | Zielwert             | Modus                       |
| -------------------------- | -------------------- | --------------------------- |
| `radio.zukkafabrik.de`     | Host-IP              | Cloudflare **DNS-only**     |
| `portal.zukkafabrik.de`    | Host-IP              | Cloudflare **DNS-only**     |
| `tapes.zukkafabrik.de`     | Host-IP              | Cloudflare **DNS-only**     |
| `ingest.zukkafabrik.de`    | Host-IP              | Cloudflare **DNS-only**     |
| `cast.zukkafabrik.de`      | Host-IP (zuletzt)    | Cloudflare **DNS-only**     |

- **DNS-only (grey cloud), kein Proxy.** Ein proxied Record bricht die
  ACME-/Edge-Annahmen und proxied den `Host`-Header-Pfad doppelt
  (`infrastructure.md` §3.3/§7).
- **`cast` folgt zuletzt:** `cast.zukkafabrik.de` zeigt bis zur Abschaltung auf
  das Bestandssystem (`infrastructure.md` §4 Frage 2, §6 Phase 6 Schritt 28).
  Erst **nach** dessen Abschaltung wird der Record auf den neuen Stack
  umgeschrieben. Der Caster-Ingest läuft bis dahin über
  `ingest.zukkafabrik.de` (`env/prod.env.example`, §3.3-Snippet).
- **TTL-Vorbereitung:** 24–48 h vor dem Fenster die TTL der vier Records auf
  **300 s** senken. Nach **≥ 7 Tagen** stabilem Betrieb wieder anheben.

**Verifikation je Record:**

```bash
dig +short radio.zukkafabrik.de @<autoritativer-Resolver>
dig +short radio.zukkafabrik.de @1.1.1.1
# Erwartung: beide = Host-IP
```

**Rollback-Seite des DNS:** der alte Zielwert (Bestandssystem-IP) ist vor dem
Umschalten in der Ausführungs-Checkliste notiert (nicht im Repo erfinden).

## 4. Caddy-Edge-Integration (Phase 6, Schritte 25–26)

Einmaliger, risikoärmster Host-Eingriff. Sequenz exakt nach
`infrastructure.md` §3.3:

1. `mkdir -p /opt/apps/caddy/sites`
2. In `/opt/scripts/caddy-planet-run.sh` **eine** Zeile ergänzen:
   `-v /opt/apps/caddy/sites:/etc/caddy/sites:ro`
3. In `/home/momo/Caddyfile` **eine** Zeile ergänzen:
   `import /etc/caddy/sites/*.caddy`
4. Snippet nach `/opt/apps/caddy/sites/zukka-prod.caddy` kopieren
   (Repo: `deploy/caddy/edge/zukka-prod.caddy`).
5. `caddy validate` **vor** dem Recreate.
6. Edge-Container **nur im Fenster** neu erzeugen.

**Vor jeder Änderung sichern:** `/home/momo/Caddyfile` und
`/opt/scripts/caddy-planet-run.sh` nach `*.bak-<datum>` kopieren und nach der
Änderung den Diff zeigen.

**Leerer Glob ist kein Fehler:** „It is an error if a specific file cannot be
found, but an empty glob pattern is not an error." (`infrastructure.md` §3.3,
belegt gegen die Caddy-Doku). Mount + `import`-Zeile dürfen also **vor** dem
Snippet-Drop erfolgen — ein noch leerer `sites`-Ordner bricht den Edge nicht.

**Abbruchkriterium:** `caddy validate` rot → Recreate **nicht** ausführen,
Änderungen zurückrollen (`*.bak-<datum>`), Fenster abbrechen.

**Betriebsrealität Edge:** Der Edge-Caddy läuft als Container `mellon-caddy`
mit `NetworkMode=host`, TLS via ACME **DNS-01** (Cloudflare)
(`infrastructure.md` §1.3, §2.5, §3.3). Ein „Reload" ist beim Edge **nicht
garantiert** (handgepflegte Datei, kein Admin-API im Runbook belegt; vgl.
`docs/deploy-prod.md` zu `admin off`). Der belegte Weg ist daher der
**Recreate** — mit **Downtime für alle Domains des Hosts** im Fenster.

**Snippet-Validität (verifizierter Befund, vor dem Drop prüfen):**

```bash
caddy validate --config deploy/caddy/edge/zukka-prod.caddy --adapter caddyfile
```

Das Repo-Artefakt nutzt die gültige **Blockform** von `request_body` und ist
mit `caddy validate` (Caddy 2.9) **grün** (Exit 0). Es weicht damit bewusst von
der ursprünglich einzeilig notierten Zeile ab, die Caddy 2.9 ablehnte
(`Unexpected next token after '{' on same line`). `docs/infrastructure.md` §3.3
wurde im selben PR auf dieselbe Blockform korrigiert, sodass Doku und
Repo-Snippet jetzt übereinstimmen.

**Verifikation nach dem Recreate:** die vier ZUKKA-Hosts antworten **und**
mindestens **zwei fremde Host-Domains** sind unverändert erreichbar
(`infrastructure.md` §6 Verifikation, §7 Nr. 12).

## 5. Compose/Env-Umschaltung

**Real implementierter Weg (Repo-Stand).** `ZUKKA_EDGE_MODE=own|shared` steuert
die Edge-Verdrahtung (Issue #91). `compose/prod.yml` bleibt die **shared**-Basis:
Caddy mit `caddy:2.9` und festem `command`
`["caddy", "run", "--config", "/etc/caddy/Caddyfile.prod", "--adapter", "caddyfile"]`,
Port-Mapping nur `127.0.0.1:${ZUKKA_PORT_CADDY:-12080}:12080`. Das Override
`compose/prod-own.yml` setzt **nur** für `caddy` das Image
`ghcr.io/caddybuilds/caddy-cloudflare:2.9`, `command` → `Caddyfile.prod-own` und
den zusätzlichen Publish `0.0.0.0:${ZUKKA_EDGE_HOST_PORT:-12443}:12443`.
`deploy/systemd/zukka-deploy` liest `ZUKKA_EDGE_MODE` aus dem PROD-env-file und
bindet das Override nur bei `own` mit `-f` ein (Caddy-Validate dann gegen
`Caddyfile.prod-own`); bei `shared` bleibt es beim bisherigen Befehl.
`Caddyfile.prod`/`Caddyfile.prod-own` setzen `auto_https off` und `admin off`
(`docs/deploy-prod.md`). Die Umschaltung `own` → `shared` ist damit ein
**Flag-Wechsel auf `shared`** plus Caddy-Recreate: der Edge übernimmt den
öffentlichen Pfad, der Stack hört weiter nur lokal auf `12080`.

**In der Doku genannter Zielzustand.** `infrastructure.md` §2.5/§3.1/§6 Phase 6
(Schritt 27) nennt `ZUKKA_EDGE_MODE=own|shared`; Ziel ist `shared`
(`Caddyfile.prod`).

**Abweichung Repo ↔ Doku (behoben, Issue #91).** `env/prod.env.example` enthält
nun `ZUKKA_EDGE_MODE=own`, `ZUKKA_EDGE_HOST_PORT=12443`, `ZUKKA_ACME_EMAIL=` und
`CLOUDFLARE_API_TOKEN=` (Platzhalter, kein Secret); der tote Key
`ZUKKA_PORT_CADDY_TLS` ist entfernt. `compose/prod-own.yml` (neu) wählt im
own-Modus `Caddyfile.prod-own` + `0.0.0.0:12443`, `deploy/systemd/zukka-deploy`
verdrahtet den Mode. Der dokumentierte `own`-Parallelbetrieb ist damit im Repo
**verdrahtet** — die frühere Vorbedingung/Blocker ist behoben. `ZUKKA_DOMAIN_CAST`
(`cast.*` vs. `ingest.*`) bleibt ein **separater** Drift, nicht Teil von #91.

**vorher / nachher:**

| Aspekt                 | vorher (`own`)                            | nachher (`shared`)              |
| ---------------------- | ----------------------------------------- | ------------------------------- |
| Öffentl. Listener      | `0.0.0.0:12443`                           | keiner (nur `127.0.0.1:12080`)  |
| TLS                    | Caddy, ACME DNS-01 (`Caddyfile.prod-own`) | Edge (ACME DNS-01)              |
| Caddy-Config           | `Caddyfile.prod-own`                      | `Caddyfile.prod`                |
| `CLOUDFLARE_API_TOKEN` | im Stack nötig                            | nur noch im Edge                |
| Öffentl. Pfad          | direkt `:12443`                           | Edge → `127.0.0.1:12080`        |

## 6. Cutover-Sequenz im Fenster (03:00–04:00 Europe/Berlin)

| Zeit  | Schritt                              | Referenz                         | Go / No-Go                                          |
| ----- | ------------------------------------ | -------------------------------- | --------------------------------------------------- |
| T-30  | Vorbedingungen + Backup              | Abschnitt 2; `restic snapshots`  | Go: alle Punkte grün, Smoke grün — sonst No-Go      |
| 03:00 | Edge-Bootstrap (Mount + `import`)    | Abschnitt 4, Schritte 1–3        | Go: `caddy validate` grün (leerer Glob ist ok)      |
| 03:05 | Snippet-Drop + `caddy validate`      | Abschnitt 4, Schritt 4–5         | Go: validate grün — sonst Abbruch (Abschnitt 4)     |
| 03:10 | Edge-Recreate + Fremd-Domain-Check   | Abschnitt 4, Schritt 6           | Go: 4 ZUKKA-Hosts ok **und** ≥ 2 Fremd-Domains ok   |
| 03:15 | Stack-Caddy-Recreate                 | `zukka-deploy prod --tag <sha>` bzw. `systemctl restart zukka-prod` | Go: Container healthy, `http://127.0.0.1:12080/__zu/health` = ok |
| 03:20 | DNS `radio`/`portal`/`tapes`         | Abschnitt 3 (Cloudflare DNS-only)| Go: `dig` liefert Host-IP                           |
| 03:25 | Verifikation                         | Abschnitt 7                      | Go: `curl -I https://radio.zukkafabrik.de/stream.mp3` = 200 |
| 03:35 | Bestandssystem aus + `cast`-DNS      | fremder Prozess, Host-Owner      | Go: erst Bestandssystem aus, **dann** `cast`-Record |
| 03:45 | Nachlauf/Beobachtung                 | `docs/monitoring.md`             | Go: keine Alarme, Hörerzahl plausibel               |
| 04:00 | Fensterende                          | —                                | danach 24 h begründungspflichtig (Abschnitt 8)      |

Jeder Schritt hat ein Go/No-Go; bei **No-Go** → Abschnitt 8.

**Keine Audio-Recreates** (`liquidsoap`/`icecast`) im Cutover: Caddy/DNS ändern
nichts am laufenden Audio-Pfad (§2.9, §5.7). Falls doch nötig → `--force` nur
mit Operator-Override und Ankündigung.

**Zeitzone:** immer `Europe/Berlin`, **nie** UTC-Offsets hartcodieren
(DST-sicher, `infrastructure.md` §2.9).

## 7. Verifikation (Abnahme im Fenster)

Jede Prüfung nennt das erwartete Ergebnis; eine Abweichung ist ein
Rollback-Trigger (Abschnitt 8).

- `curl -I https://radio.zukkafabrik.de/stream.mp3` → **200**.
- HLS-Playlist jünger als 3× Segmentdauer:
  `https://radio.zukkafabrik.de/hls/index.m3u8`.
- `portal`/`tapes` `/healthz` → 200; `cast` liefert Seite bzw. `/healthz`.
- `docker compose ps` → alle `running (healthy)`.
- `scripts/smoke.sh prod` → „all checks passed".
- Listener-Zahl in `zukka_icecast_listeners` sichtbar (§7 Nr. 11/12).
- Fremd-Domains des Hosts unverändert (Stichprobe ≥ 2, §6 Verifikation).
- Deploy-Gate-Gegenprobe — **nur beobachten, kein Deploy**:
  `sudo /usr/local/bin/zukka-deploy prod --check-window` bei `listeners > 0`
  endet mit **Exit 3** (§7 Nr. 14).
- Nachlauf-Beobachtung: `docs/monitoring.md` — `ZukkaAudioDown`/
  `ZukkaIcecastDown` (keine neue Alert-Definition in #45).

## 8. Rollback-Fenster und Rollback-Trigger

**Fenster:** Cutover-Fenster **plus 24 h**. Konkret: Ausführung 03:00–04:00 am
Cutover-Tag; Rückweg bis **04:00 desselben Fensters ohne Diskussion**; danach
**24 h** „begründungspflichtig" (bis 04:00 des Folgetags). Datum in der
Ausführungs-Checkliste eintragen.

**Trigger — automatisch/sofort:**

- `/stream.mp3` nicht 200 innerhalb 2 min.
- `caddy validate` rot.
- Eine fremde Host-Domain nicht erreichbar.
- Portal/Tapes `/healthz` rot nach dem Caddy-Recreate.
- `ZukkaAudioDown` ausgelöst.

**Trigger — manuell (Operator-Entscheid):**

- Hörerzahl bricht gegenüber dem Parallelbetrieb ein.
- Caster-Ingest über `cast` scheitert.
- Edge-Latenz auffällig.

**Rückweg Edge/DNS** (Reihenfolge so, dass die Fremd-Domains zuerst sicher
sind):

1. `import`-Zeile aus `/home/momo/Caddyfile` entfernen bzw. Snippet aus
   `/opt/apps/caddy/sites/` entfernen.
2. `caddy validate`.
3. Edge-Container (`mellon-caddy`) neu erzeugen.
4. DNS zurück auf den alten Zielwert (Abschnitt 3).
5. Verifikation wie Abschnitt 7.

**Rückweg Stack:** `sudo /usr/local/bin/zukka-deploy prod --rollback`
(Mechanik: `.deployed.prev`, Smoke-Gate) — **nur referenziert**, nicht neu
implementiert. Details: `docs/deploy-prod.md` „Rollback", Restore:
`docs/backup.md`.

**Kein automatischer DNS-Rollback:** DNS ist manuell (Cloudflare) und wird mit
Rück-Wert in der Ausführungs-Checkliste dokumentiert; kein Skript rollt DNS
zurück.

**Abgrenzung #47:** #47 beschreibt den vollständigen Rollback-Plan inkl.
Daten-/Migrations-Rückweg (`docs/rollback.md`); hier nur Fenster, Trigger und
operative Reihenfolge.
