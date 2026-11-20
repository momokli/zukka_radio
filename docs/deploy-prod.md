# PROD-Deploy (Issue #41)

Runbook für den PROD-Pfad: Release-Tag → GitHub Environment `prod` mit
**Required Reviewer** → Deploy per SSH-Wrapper `zukka-deploy` → Smoke-Gate →
`.deployed`. Rollback über `.deployed.prev`.

Grundlage: `docs/consolidated-architecture.md` §5.7–5.9,
`docs/infrastructure.md` §2.8–2.9/§3.5.

## Abgrenzung

| Thema                            | Issue | Status                                       |
| -------------------------------- | ----- | -------------------------------------------- |
| DEV-Deploy (`deploy-dev.yml`)    | #40   | gemergt                                      |
| PROD-Deploy (dieses Runbook)     | #41   | Tag/Release, Reviewer-Gate, Smoke, Rollback  |
| Audience-/Fenster-Gate           | #42   | **nicht** enthalten                          |
| Pre-Deploy-Backup                | #43   | **nicht** enthalten                          |

`zukka-deploy prod --check-window` (Scheduler `GET /internal/ops/audience`,
Fenster 03:00–04:00 Berlin) existiert noch nicht; der Workflow hat an der
Stelle einen Kommentar, #42 ergänzt den Aufruf vor dem Deploy. Ein
Pre-Deploy-Snapshot (`zukka-backup predeploy <sha>`) kommt mit #43 in den
Wrapper vor den `pull`.

## Host-Voraussetzungen

Layout (`docs/infrastructure.md` §3.2):

```text
/opt/apps/radio-zukka/
├── repo/          # git checkout (compose/, deploy/caddy/, scripts/)
├── dev/           # .deployed, .deployed.prev
└── prod/          # .deployed, .deployed.prev, compose/, env/, scripts/
/etc/zukka/
├── prod.env       # 0600 root:zukka
└── ...
```

- `prod/compose/prod.yml` und `prod/env/prod.env` müssen vorhanden sein —
  der Wrapper bricht sonst mit „missing …" ab.
- `prod/env/prod.env` enthält `ZUKKA_IMAGE_REGISTRY` und `ZUKKA_TAG`; den Tag
  überschreibt der Wrapper pro Deploy per `export ZUKKA_TAG=<sha>`.
- GHCR-Login einmalig als root: `docker login ghcr.io` mit einem Token, das
  `read:packages` darf (Host-Secret, nicht in GitHub).
- `sudoers`: `zukka-deploy ALL=(root) NOPASSWD: /usr/local/bin/zukka-deploy`
  (exakter Pfad; Argumente validiert das Skript).
- Der Wrapper liegt als `deploy/systemd/zukka-deploy` im Repo und wird nach
  `/usr/local/bin/zukka-deploy` (root:root, 0755) installiert.
- Caddy läuft mit `Caddyfile.prod`: `auto_https off`, `admin off`,
  Listener `:12080` hinter dem Edge-Caddy. Deshalb prüft der Smoke-Test
  HTTP statt TLS und hat keinen Admin-Check.

## Required Reviewer für Environment `prod`

Einmalig im Repository (Owner/Repo ggf. anpassen):

```bash
# Reviewer-IDs ermitteln
gh api users/<login> --jq .id

# Environment anlegen/aktualisieren — Reviewer + Tag-Policy
gh api -X PUT repos/momokli/zukka_radio/environments/prod \
  --input - <<'JSON'
{
  "reviewers": [
    { "type": "User", "id": 12345678 }
  ],
  "deployment_branch_policy": {
    "protected_branches": false,
    "custom_branch_policies": true
  }
}
JSON

# Nur Release-Tags dürfen in `prod` deployen
gh api -X POST repos/momokli/zukka_radio/environments/prod/deployment-branch-policies \
  -f name='v*' -f type=tag
```

Hinweise:

- `PUT` ersetzt die Environment-Konfiguration. Vorhandene Reviewer zuerst per
  `GET repos/momokli/zukka_radio/environments/prod` lesen und mit übergeben.
- Team-Reviewer: `{"type": "Team", "id": <team_id>}`.
- Secret `DEPLOY_SSH_KEY` als Environment-Secret (`prod`) oder Repo-Secret
  hinterlegen; der Workflow liest `secrets.DEPLOY_SSH_KEY`.
- Die Tag-Policy ist der zweite Riegel: ein versehentlicher Branch-Deploy
  scheitert schon an GitHub, bevor der Reviewer gefragt wird.

## Tag/Release-Flow

1. Release-Tag auf einem Commit setzen, der Ancestor von `origin/v2` ist:

   ```bash
   git tag -a v2026.11.20 -m "Release v2026.11.20" <sha>
   git push origin v2026.11.20
   ```

2. Der Push triggert `.github/workflows/deploy-prod.yml`:
   - Job `release` prüft `git merge-base --is-ancestor <sha> origin/v2` und
     erstellt das GitHub Release (`gh release create`, Notes aus dem
     Tag-Message; leere Notes → „Release <tag>").
   - Job `deploy` wartet auf den Required Reviewer im Environment `prod`.

Ein Tag außerhalb von `v2` bricht den `release`-Job ab, `deploy` läuft dann
nicht.

## Deploy

Automatisch per Tag-Push (siehe oben) oder manuell:

```bash
gh workflow run deploy-prod.yml \
  -f tag=v2026.11.20        # leer = github.sha
  -f force=false            # true = liquidsoap/icecast mitrestarten
```

`force=true` ist hörbar (2–5 s Stille bzw. Reconnect aller Hörer) und laut
Architektur max. 1×/Tag erlaubt. Der Workflow löst den Input per
`git rev-parse <tag>^{commit}` auf und ruft auf dem Host auf:

```bash
sudo /usr/local/bin/zukka-deploy prod --tag <sha> [--force]
```

Wrapper-Ablauf:

1. `validate_tag` (`^[0-9a-f]{7,40}$`), Pfade prüfen.
2. `compose -f prod/compose/prod.yml --env-file prod/env/prod.env pull`
   für `scheduler portal tapes` (mit `--force` zusätzlich `icecast`;
   `liquidsoap` bleibt digest-/tag-gepinnt).
3. `up -d --no-deps --wait` für `scheduler` → `portal` → `tapes`.
4. `caddy validate` + `caddy reload` mit `/etc/caddy/Caddyfile.prod`
   (kein Admin-API-Reload, `admin off`).
5. `liquidsoap`/`icecast` **nur** mit `--force`.
6. `compose up -d --wait` (Konvergenz).
7. `scripts/smoke.sh prod` als Gate — erst danach wird der Status geschrieben.
8. `.deployed` → `.deployed.prev` rotieren, dann neues `.deployed` atomar
   (`.deployed.next` + `mv`) mit `sha`, `image_tag`, `timestamp`, `ref`.

Der Workflow ruft nach dem Wrapper noch einmal
`/opt/apps/radio-zukka/repo/scripts/smoke.sh prod` auf (PROD-Ports sind
loopback-only, der Runner kommt nicht direkt dran).

Smoke-Prüfungen (`scripts/smoke.sh prod`, Defaults, Overrides über
`ZUKKA_SMOKE_*`):

| Check                  | Ziel                                                      |
| ---------------------- | --------------------------------------------------------- |
| Caddy-Liveness         | `http://127.0.0.1:12080/__zu/health`                      |
| Portal `/healthz`      | `http://portal.zukkafabrik.de:12080` per `--resolve`      |
| Tapes `/healthz`       | `http://tapes.zukkafabrik.de:12080` per `--resolve`       |
| Scheduler `/healthz`   | `http://127.0.0.1:12010/healthz`                          |
| Scheduler `/readyz`    | `http://127.0.0.1:12010/readyz`                           |
| HLS `index.m3u8`       | non-fatal WARN (Liquidsoap, #84)                          |

## Rollback

`.deployed.prev` enthält den vorherigen Stand. Rollback deployt diesen SHA
mit demselben Ablauf inklusive Smoke-Gate:

```bash
gh workflow run deploy-prod.yml -f rollback=true
# oder auf dem Host:
sudo /usr/local/bin/zukka-deploy prod --rollback
```

Der Wrapper liest `sha=` aus `.deployed.prev` und ruft intern
`deploy_prod <sha> 0` auf. Nach grünem Smoke rotiert der Status erneut:
`.deployed` (der fehlerhafte Stand) wird zu `.deployed.prev`, der alte Stand
zu `.deployed`. Ein zweiter Rollback toggelt also zurück.

Voraussetzung: Migrationen sind expand/contract — die alte Binary muss mit dem
neuen Schema starten (`docs/consolidated-architecture.md` §5.7).

## Troubleshooting

- **„no …/.deployed.prev to roll back to"** — es gab noch keinen erfolgreichen
  PROD-Deploy; mit `--tag` deployen.
- **GHCR `denied`** — `docker login ghcr.io` auf dem Host erneuern
  (`read:packages`).
- **Smoke rot, Deploy abgebrochen** — `.deployed` bleibt unverändert; Logs:
  `docker compose -f /opt/apps/radio-zukka/prod/compose/prod.yml logs <svc>`.
- **Reviewer-Gate blockiert workflow_dispatch** — Environment `prod` im
  GitHub-UI unter Settings → Environments prüfen (Reviewer + Tag-Policy).