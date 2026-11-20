# PROD-Deploy (Issue #41)

Runbook für den PROD-Pfad: Release-Tag → GitHub Environment `prod` mit
**Required Reviewer** → Deploy per SSH-Wrapper `zukka-deploy` → Smoke-Gate →
`.deployed`. Rollback über `.deployed.prev`.

Grundlage: `docs/consolidated-architecture.md` §5.7–5.9,
`docs/infrastructure.md` §2.8–2.9/§3.2/§3.5.

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

Layout (`docs/infrastructure.md` §3.2): der Git-Checkout enthält **Compose,
Caddy und Skripte**; die Env-Verzeichnisse `dev/`/`prod/` tragen **nur** den
Deploy-Status:

```text
/opt/apps/radio-zukka/
├── repo/          # git checkout (compose/, deploy/caddy/, scripts/, deploy/systemd/)
├── dev/           # .deployed, .deployed.prev
└── prod/          # .deployed, .deployed.prev   (sonst nichts)
/etc/zukka/
├── prod.env       # 0600 root:zukka
└── ...
```

- Der Wrapper nutzt `${APP_ROOT}/repo/compose/prod.yml` und
  `${APP_ROOT}/repo/scripts/smoke.sh`. Vor jedem Deploy synchronisiert er den
  Checkout (`git fetch --all --tags --prune` + `git checkout --force --detach
  <sha>`), sofern `${APP_ROOT}/repo/.git` existiert. Änderungen an
  `compose/prod.yml`/`scripts/smoke.sh` greifen so mit dem Release-Commit.
- Secrets und Compose-Env liegen in `/etc/zukka/prod.env`. Der Wrapper setzt
  `export ZUKKA_ENV_FILE=/etc/zukka/prod.env`; `compose/prod.yml` liest
  `env_file: ${ZUKKA_ENV_FILE:-../env/prod.env}`. CI bleibt beim relativen
  Default `../env/prod.env` grün.
- `prod.env` enthält `ZUKKA_IMAGE_REGISTRY` und einen `ZUKKA_TAG`-Platzhalter;
  den echten Tag setzt der Wrapper pro Deploy per `export ZUKKA_TAG=<sha>`.
- GHCR-Login einmalig als root: `docker login ghcr.io` mit einem Token, das
  `read:packages` darf (Host-Secret, nicht in GitHub).
- `sudoers`: `zukka-deploy ALL=(root) NOPASSWD: /usr/local/bin/zukka-deploy`
  (exakter Pfad; Argumente validiert das Skript).
- Der Wrapper liegt als `deploy/systemd/zukka-deploy` im Repo und wird nach
  `/usr/local/bin/zukka-deploy` (root:root, 0755) installiert.
- Caddy läuft mit `Caddyfile.prod`: `auto_https off`, `admin off`,
  Listener `127.0.0.1:12080` hinter dem Edge-Caddy. Weil `admin off` die
  Admin-API (`localhost:2019`) abschaltet, ist `caddy reload` in PROD
  **unmöglich**; der Wrapper validiert die Datei und **recreatet** den
  Caddy-Container (`up -d --no-deps --force-recreate --wait caddy`).
  `infrastructure.md` §2.2 nennt intern `:12081` — das gilt nur, wenn die
  Admin-API aktiv ist; in PROD ist sie aus, die Liveness läuft über
  `:12080/__zu/health`. Deshalb prüft der Smoke-Test HTTP statt TLS und hat
  keinen Admin-Check.

## Required Reviewer für Environment `prod`

Einmalig im Repository (Owner/Repo ggf. anpassen):

```bash
# Reviewer-IDs ermitteln
gh api users/<login> --jq .id

# Environment anlegen/aktualisieren — Reviewer + Branch-Policy-Grundgerüst
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

# Release-Tags dürfen deployen …
gh api -X POST repos/momokli/zukka_radio/environments/prod/deployment-branch-policies \
  -f name='v*' -f type=tag

# … und der Dispatch-Branch v2 (Run-Ref bei workflow_dispatch ist ein Branch).
gh api -X POST repos/momokli/zukka_radio/environments/prod/deployment-branch-policies \
  -f name='v2' -f type=branch
```

Hinweise:

- Beide Policies sind nötig: ein Tag-Push läuft als `refs/tags/v*`, ein
  `workflow_dispatch` läuft als `refs/heads/v2` (Default-Branch). Ohne die
  Branch-Policy lehnt GitHub jeden Dispatch **vor** dem Reviewer ab.
- `PUT` ersetzt die Environment-Konfiguration. Vorhandene Reviewer zuerst per
  `GET repos/momokli/zukka_radio/environments/prod` lesen und mit übergeben.
- Team-Reviewer: `{"type": "Team", "id": <team_id>}`.
- Secret `DEPLOY_SSH_KEY` als Environment-Secret (`prod`) oder Repo-Secret
  hinterlegen; der Workflow liest `secrets.DEPLOY_SSH_KEY`.
- Optional `DEPLOY_KNOWN_HOSTS`: Inhalt der Host-Key-Zeile
  (`ssh-keyscan -H cast.zukkafabrik.de`) als Secret hinterlegen. Ist es
  gesetzt, schreibt der Workflow es nach `known_hosts` (gepinnt); sonst fällt er
  auf `ssh-keyscan` zurück (TOFU, nur Vertrauen beim ersten Kontakt).

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
   - Job `deploy` prüft denselben Ancestor-Check erneut (nicht beim Rollback)
     und wartet dann auf den Required Reviewer im Environment `prod`.

Ein Tag außerhalb von `v2` bricht den `release`-Job ab; der `deploy`-Job wird
dann über `needs` **übersprungen** (er läuft nicht) — nicht „schlägt
 ebenfalls fehl". Der eigene Ancestor-Check im `deploy`-Job greift daher nur bei
`workflow_dispatch`, wo kein `release`-Job vorgeschaltet ist.

## Deploy

Automatisch per Tag-Push (siehe oben) oder manuell:

```bash
gh workflow run deploy-prod.yml \
  -f tag=v2026.11.20        # leer = github.sha
  -f force=false            # true = liquidsoap/icecast mitrestarten
```

`workflow_dispatch` läuft als Branch `v2` (Run-Ref `refs/heads/v2`); das ist
durch die zusätzliche Branch-Policy am Environment `prod` erlaubt (siehe oben).
Der Tag-Input wird per `git rev-parse <tag>^{commit}` aufgelöst und muss
Ancestor von `origin/v2` sein. Der Workflow ruft auf dem Host auf:

```bash
sudo /usr/local/bin/zukka-deploy prod --tag <sha> [--force] --ref "$GITHUB_REF"
```

`--ref` wird nur im `.deployed`-State protokolliert (`refs/tags/v*` bei
Tag-Push, `refs/heads/v2` bei Dispatch); Default ist `refs/heads/v2`. Bewusste
Vereinfachung: bei `workflow_dispatch` ist `$GITHUB_REF` der **Branch-Ref**
(`refs/heads/v2`), auch wenn `tag`-Input einen Tag-SHA deployt — nur der
Tag-Push liefert `refs/tags/v*`.

Wrapper-Ablauf:

1. `validate_tag` (`^[0-9a-f]{7,40}$`), Pfade prüfen, `sync_repo` (Checkout auf
   den Target-SHA).
2. `compose -f repo/compose/prod.yml --env-file /etc/zukka/prod.env pull`
   für `scheduler portal tapes` (mit `--force` zusätzlich `icecast`;
   `liquidsoap` bleibt digest-/tag-gepinnt).
3. `up -d --no-deps --wait` für `scheduler` → `portal` → `tapes`.
4. `caddy validate` mit `/etc/caddy/Caddyfile.prod`, dann Recreate:
   `up -d --no-deps --force-recreate --wait caddy`
   (kein Admin-API-Reload, `admin off`).
5. `liquidsoap`/`icecast` **nur** mit `--force`.
6. `compose up -d --wait scheduler portal tapes caddy` (Konvergenz **ohne**
   Audio — ein frischer `ZUKKA_TAG` darf `icecast`/`liquidsoap` nicht implizit
   recreaten).
7. `repo/scripts/smoke.sh prod` als Gate.
8. Bei grünem Smoke: `.deployed.next` schreiben, `.deployed` →
   `.deployed.prev` rotieren, `.next` → `.deployed` (crash-atomar). Bei rotem
   Smoke und vorhandenem `.deployed`: automatischer Rollback auf `.deployed`
   (siehe unten), dann Exit ≠ 0.

Vor jedem Deploy/Rollback promotioniert `recover_state` ein verbliebenes
`.deployed.next`, falls `.deployed` fehlt (Crash zwischen Rotation und
Promotion), und protokolliert das.

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

`.deployed.prev` ist der **letzte bekannt gute** Stand und wird durch einen
Rollback **nicht** rotiert. Der Rollback deployt diesen SHA mit demselben
Ablauf inklusive Smoke-Gate und **setzt nur `.deployed`** auf den alten SHA:

```bash
gh workflow run deploy-prod.yml -f rollback=true
# oder auf dem Host:
sudo /usr/local/bin/zukka-deploy prod --rollback
```

Der Wrapper liest `sha=`/`ref=` aus `.deployed.prev` und ruft intern
`deploy_prod <sha> 0 <ref> 1` auf (Rollback-Modus). Weil `.deployed.prev`
unverändert bleibt, ist ein zweiter `--rollback` **idempotent**: er deployt
denselben guten Stand erneut — kein Zurück-Togglen auf den defekten SHA.

**Automatischer Rollback:** Schlägt das Smoke-Gate eines normalen Deploys fehl
und existiert ein `.deployed` mit einem SHA ≠ dem fehlgeschlagenen Tag, deployt
der Wrapper automatisch den SHA aus `.deployed` — den zuletzt erfolgreich
ausgerollten, aktuell laufenden Stand (nicht `.deployed.prev`, den Stand
davor; der bleibt unangetastet) — mit gleichem Smoke-Gate, ohne erneute
Rotation, und bricht danach mit Exit ≠ 0 ab. Der Deployzustand bleibt so
konsistent mit `.deployed`. Beim ersten Deploy (kein `.deployed`) findet kein
automatischer Rollback statt. Eine Endlos-Rekursion ist ausgeschlossen
(Rollback-Modus rekursiert nicht).

Voraussetzung: Migrationen sind expand/contract — die alte Binary muss mit dem
neuen Schema starten (`docs/consolidated-architecture.md` §5.7).

## Troubleshooting

- **„no …/.deployed.prev to roll back to"** — es gab noch keinen erfolgreichen
  PROD-Deploy; mit `--tag` deployen.
- **GHCR `denied`** — `docker login ghcr.io` auf dem Host erneuern
  (`read:packages`).
- **Smoke rot, Deploy abgebrochen** — bei vorhandenem `.deployed` (SHA ≠
  fehlgeschlagener Tag) wurde automatisch auf diesen guten Stand zurückgerollt;
  beim ersten Deploy (kein `.deployed`) findet kein Auto-Rollback statt. Logs:
  `docker compose -f /opt/apps/radio-zukka/repo/compose/prod.yml logs <svc>`.
- **Reviewer-Gate blockiert workflow_dispatch** — Environment `prod` im
  GitHub-UI unter Settings → Environments prüfen (Reviewer + **beide**
  Policies: Tag `v*` und Branch `v2`).
