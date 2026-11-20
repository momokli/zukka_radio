# Monitoring (M6 — Issue #44)

Runbook für die Metriken, Alarme und die Anbindung an die **geteilte**
Monitoring-Kette (`mellon-prometheus`). Grundlage: `docs/infrastructure.md`
§2.11/§3.2/§3.8 und `docs/consolidated-architecture.md` §3.8/§4.2.

> **Kein eigener Prometheus** (§2.11). Der Stack liefert Metriken auf `:9091`,
> die der Host-Prometheus über die Caddy-Brücke `:12091` im Netz
> `mellon-network` scrapt. Das Repo liefert die Artefakte; die **einmalige**
> Einbindung in den geteilten Stack macht der Host-Owner (siehe
> [Shared-Änderungen](#shared-nderungen-host-owner)).

## Überblick

```
scheduler :9091  ──(back/egress)──▶  caddy :12091  ──(mellon-network)──▶  mellon-prometheus
  /metrics                            reverse_proxy                        job: zukka-prod
  /healthz
```

- `GET /metrics` (Prometheus-Textformat `text/plain; version=0.0.4`) wird
  **zusätzlich** am Hauptrouter (`:8080`, §4.2) und auf dem dedizierten
  Listener `ZUKKA_METRICS_BIND` (Default `0.0.0.0:9091`) ausgeliefert.
- Der Scrape-Pfad ist bewusst **ohne Auth** und läuft über `:9091`/`:12091`.
- `caddy` ist der **einzige** Container im geteilten Netz (§3.2).

## Metriken

| Metrik                            | Typ     | Bedeutung                                                              | Hinweis |
| --------------------------------- | ------- | ---------------------------------------------------------------------- | ------- |
| `zukka_icecast_listeners`         | gauge   | Hörer gesamt laut Icecast `status-json.xsl`                            | Queue/Gate/Metrik nutzen dieselbe Quelle; nicht erreichbar ⇒ `0` |
| `zukka_live_sources`              | gauge   | Laufende Live-Quellen (Live-Broadcasts + aktiver Override)             | |
| `zukka_source_state{layer}`       | gauge   | `1` für die aktive Ebene, sonst `0`                                    | 6 Ebenen: `live,show,queue,rotation,relay,fun` |
| `zukka_queue_fill_seconds{queue}` | gauge   | Pending-Spielzeit je Queue in Sekunden                                 | **Approximation**: Summe `tracks.duration_ms` pending `track`-Items / 1000; Items ohne Track zählen `0` (Untergrenze). Label = Queue-**id** |
| `zukka_hls_segment_age_seconds`   | gauge   | Alter des neuesten HLS-Segments in `ZUKKA_HLS_DIR`                     | **fehlt**, solange keine Segmente existieren (#84) — kein Fake-Wert |
| `zukka_music_api_errors_total`    | counter | Fehler beim Poll der externen Autorität **seit Prozessstart**          | Der Scheduler hat keinen eigenen Music-API-Client (der lebt im Portal, §3.5); gezählt wird der Poll der externen Stream-Autorität (Icecast) |
| `zukka_scheduler_db_locked_total` | counter | SQLite `busy`/`locked` **seit Prozessstart**                           | Inkrement zentral im `sqlx::Error → ApiError`-Mapping |
| `zukka_build_info{version,sha}`   | gauge=1 | Build-Version/SHA                                                     | `ZUKKA_VERSION`/`ZUKKA_GIT_SHA`, Fallback `CARGO_PKG_VERSION` (sha dann leer) |

**Counter-Reset:** Die Zähler sind prozesslokal (`AtomicU64`) und beginnen bei
jedem Start bei `0`. `rate()`/`increase()` behandeln einen Neustart wie einen
Counter-Reset — die `# HELP`-Texte weisen darauf hin.

## Alarme

Alle acht Alarme liegen im Block `radio_zukka` (`deploy/monitoring/rules.yml`).

| Alarm                         | Bedingung (Kurz)                                                        | Schwere   |
| ----------------------------- | ----------------------------------------------------------------------- | --------- |
| `ZukkaAudioDown`              | `zukka_hls_segment_age_seconds > 20` für 1 m                            | critical  |
| `ZukkaIcecastDown`            | `up{job=~"zukka-.*"} == 0` für 2 m                                      | critical  |
| `ZukkaNoListenersInBroadcast` | `zukka_icecast_listeners == 0 and zukka_live_sources > 0` für 5 m       | warning   |
| `ZukkaQueueUnderrunRisk`      | `zukka_queue_fill_seconds < 30` bei aktiver `queue`-Ebene für 2 m       | warning   |
| `ZukkaDiskPressure`           | node-exporter: `(1 - avail/size) * 100 > 85` auf `/` für 5 m            | warning   |
| `ZukkaBackupFailed`           | `node_systemd_unit_state{name="zukka-backup.service",state="failed"} == 1` | critical |
| `ZukkaCertExpiry`             | blackbox `probe_ssl_earliest_cert_expiry - time() < 14 d`               | warning   |
| `ZukkaRestartLoop`            | `changes(container_start_time_seconds[15m]) > 2` (cadvisor)             | warning   |

> `ZukkaAudioDown` ist inaktiv, solange HLS nichts produziert (#84); bis dahin
> trägt `ZukkaIcecastDown` (`up`) den Stream-Down-Alarm.

## Blackbox-Ziele

`deploy/monitoring/targets.yml` (file-SD für den Job `zukka-blackbox`):

| Ziel                                     | Modul        | Zweck          |
| ---------------------------------------- | ------------ | -------------- |
| `https://radio.<dom>/hls/index.m3u8`     | `http_2xx`   | HLS-Playlist   |
| `https://portal.<dom>/healthz`           | `http_2xx`   | Portal lebt    |
| `https://tapes.<dom>/healthz`            | `http_2xx`   | Tapes lebt     |
| `https://radio.<dom>`                    | `https_cert` | Zert-Ablauf    |

Die Module (`http_2xx`, `https_cert`) liegen in `deploy/monitoring/blackbox.yml`.

## Artefakte im Repo

| Datei                                   | Zweck                                             |
| --------------------------------------- | ------------------------------------------------- |
| `deploy/monitoring/prometheus.yml`      | Job-**Snippet** (`zukka-prod` + Blackbox + `rule_files`) |
| `deploy/monitoring/rules.yml`           | Regelblock `radio_zukka` (8 Alarme)               |
| `deploy/monitoring/blackbox.yml`        | Blackbox-Module                                   |
| `deploy/monitoring/targets.yml`         | Blackbox-Ziele (file-SD)                          |
| `deploy/monitoring/dashboard.json`      | Grafana-Dashboard                                 |
| `scripts/zukka-monitoring-check.sh`     | Offline-Validierung (CI-Job `monitoring`)         |

## Shared-Änderungen (Host-Owner)

Einmalig am geteilten Stack (§3.8) — **nicht** durch dieses Repo ausgeführt:

1. **`prometheus.yml`**: Job aus `deploy/monitoring/prometheus.yml` übernehmen
   (Ziel `zukka-prod-caddy:12091`; Netz `mellon-network` ist vorhanden).
2. **`rules.yml`**: den Block aus dem Repo einbinden.
3. **Blackbox-Ziele**: `targets.yml` + Module `blackbox.yml` ergänzen.
4. **Grafana**: `deploy/monitoring/dashboard.json` importieren.
5. **Reload-Mechanismus verifizieren** (Q6): `POST /-/reload` nur bei
   `--web.enable-lifecycle`, sonst `kill -HUP <pid>` bzw. ansible-verwaltete
   Config → dann gehört der Job **dorthin**, nicht in die Datei.

**Voraussetzungen im Shared-Stack (offene Punkte, §3.8):**

- `mellon-network`-Mitgliedschaft von `zukka-prod-caddy` und auflösbarer Name
  `zukka-prod-caddy` (Shared-Host-Fakt, außerhalb des Repos).
- **node-exporter** für `ZukkaDiskPressure` (`node_filesystem_*`) und
  `ZukkaBackupFailed` (`node_systemd_unit_state`); **cadvisor** für
  `ZukkaRestartLoop` (`container_start_time_seconds`). Präsenz/Job-Namen sind
  unbestätigt → beim Einbinden prüfen.
- **Build-Wiring** `ZUKKA_VERSION`/`ZUKKA_GIT_SHA` (Dockerfile-`ARG` + CI), sonst
  bleibt das `sha`-Label leer.

## Verifikation

```bash
# Endpoint (im Netz des Schedulers):
curl -fsS http://scheduler:9091/metrics
# oder über die Brücke:
curl -fsS http://zukka-prod-caddy:12091/metrics

# Artefakte offline prüfen (wie CI):
./scripts/zukka-monitoring-check.sh
```

Erwartet: `200` und `text/plain; version=0.0.4` mit `# HELP`/`# TYPE` je Metrik
und den sechs `zukka_source_state{layer=…}`-Zeilen.

## Troubleshooting

| Symptom | Ursache / Abhilfe |
| ------- | ----------------- |
| Scrape `up == 0` für `zukka-prod` | caddy nicht im `mellon-network`, Name `zukka-prod-caddy` nicht auflösbar, oder `:12091` nicht `expose`d. `docker network inspect mellon-network` prüfen. |
| `/metrics` liefert nur Icecast/`0`-Werte | DB-Queries schlagen fehl oder Icecast nicht erreichbar → Logs des Schedulers auf `metrics:`-Warnungen prüfen (Scrape gibt nie `5xx`). |
| `zukka_hls_segment_age_seconds` fehlt | Es gibt keine HLS-Segmente (`ZUKKA_HLS_DIR` leer/fehlt) — erwartet, bis #84 produziert. |
| `ZukkaQueueUnderrunRisk` dauerhaft | Queue ist aktive Ebene mit < 30 s Pending-Inhalt → Library/Queue füllen. |
| Counter springt auf `0` | Scheduler wurde neu gestartet (Counter sind prozesslokal) — normal, `rate()` behandelt es als Reset. |
