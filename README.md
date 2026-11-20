# radio zukka — v2

Neuer Stack für radio zukka. Diese Branch (`v2`) enthält **nur die neue Welt**.
Die alte Welt liegt auf `main` und wird hier nicht referenziert.

## Komponenten

| Komponente | Rolle | Sprache |
| --- | --- | --- |
| **scheduler** | Queue, Rotation, Schedule, Prioritäten, Live-Auswahl | Rust |
| **portal** | Caster-UI (Auth, Upload, Library, Queue, Control Panel) | Rust |
| **tapes** | öffentlicher Recordings-Feed | Rust + statisch |
| **liquidsoap** | Audio-Engine (Harbor, Sources, Encoding, Recording) | Liquidsoap |
| **icecast** | Streaming-Server | C |
| **caddy** | Reverse Proxy, HLS-Segmente, statische Dateien | Go |

## Externe Autoritäten (konsumiert, nicht deployt)

| Service | Rolle |
| --- | --- |
| **music-api** | Object Storage + Track-Autorität (ISRC, Downloads, Metadaten) |
| **music-hub** | Multi-User-Track-Verwaltung (UI/DB über music-api) |

## Dokumente

| Datei | Inhalt |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | Gesamtarchitektur, Datenfluss, Kernbegriffe |
| [`docs/data-model.md`](docs/data-model.md) | SQLite-Schema + API-Contracts |
| [`docs/liquidsoap.md`](docs/liquidsoap.md) | Liquidsoap-Script, Sources, Prioritäten |
| [`docs/portal.md`](docs/portal.md) | Auth, Upload, Library, Queue, Control Panel |
| [`docs/infrastructure.md`](docs/infrastructure.md) | Env, Caddy, systemd, CI/CD |
| [`docs/cutover.md`](docs/cutover.md) | PROD-Cutover-Runbook (DNS, Edge, Fenster, Rollback) |
| [`docs/tapes.md`](docs/tapes.md) | öffentlicher Feed, Waveforms |

## Grundsätze

1. **Zero Downtime** für Queue-, Schedule- und Live-Änderungen.
2. **Berlin-Zeit** überall (DST-korrekt).
3. **Music API** ist die einzige Datei-Autorität.
4. **RBAC** von Anfang an, permissiv by default.
5. **Kein Delay** — so instant wie möglich.
