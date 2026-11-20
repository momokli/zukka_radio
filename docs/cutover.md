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

Das Repo-Artefakt ist byte-genau der Block aus `infrastructure.md` §3.3. Die
dort einzeilig notierte Zeile `request_body { max_size 2GB }` wird von Caddy
**2.9** abgelehnt:

```text
Error: adapting config using caddyfile: Unexpected next token after '{' on same line, at .../zukka-prod.caddy:4
```

Gleiches Ergebnis im Kombi-Kontext (Wegwerf-Import in ein gültiges Caddyfile).
Gültig ist die Blockform:

```caddyfile
	request_body {
		max_size 2GB
	}
```

→ **Offener Punkt / Blocker:** §3.3 in `docs/infrastructure.md` auf die
Blockform korrigieren (eigenes Issue), **bevor** das Snippet auf den Edge
kopiert wird. Sonst greift das Abbruchkriterium oben und der Cutover stoppt
korrekt — aber ungewollt.

**Verifikation nach dem Recreate:** die vier ZUKKA-Hosts antworten **und**
mindestens **zwei fremde Host-Domains** sind unverändert erreichbar
(`infrastructure.md` §6 Verifikation, §7 Nr. 12).
