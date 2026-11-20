# Backup & Restore (Issue #43)

Runbook für den Backup-/Restore-Pfad des PROD-Stacks: DB-konsistente
restic-Snapshots (täglich), Pre-Deploy-Snapshot, wöchentlicher Restore-Drill
und Retention. Grundlage: `docs/consolidated-architecture.md` §5.9,
`docs/infrastructure.md` §2.10/§3.6/§3.7.

## Überblick

| Thema          | Wert                                                                  |
| -------------- | --------------------------------------------------------------------- |
| Umfang         | **eine** DB `zukka.db`, `recordings_staging` (ausstehende Uploads), `/etc/zukka/*.env`, `.deployed` |
| Mechanik       | DB-konsistent via `sqlite3 <db> ".backup"` in ein Staging-Verzeichnis, dann `restic backup` |
| restic-Repo    | `rclone:sbox01:/home/Backups/restic-planet-zukka` (isoliert vom generischen planet-Backup) |
| Passwort       | `/etc/zukka/restic.pw` (0600 root)                                     |
| Tag            | `zukka` (Pre-Deploy zusätzlich `predeploy-<sha>`)                       |
| Retention      | 14 daily, 8 weekly, 6 monthly (sonntags 05:00 Berlin)                   |
| **RPO**        | ≤ 24 h (täglich 04:15 Berlin)                                           |
| **RTO**        | < 30 min (vom Drill geprüft)                                            |
| Drill          | wöchentlich, Sonntag 05:30 Berlin                                       |

Nicht gesichert (regenerierbar): `hls`, `rotation`, Icecast-Logs.

Es gibt genau **eine** DB (`zukka.db`, C84) — nicht mehr `scheduler.db` +
`portal.db`. Die älteren `scheduler_data`/`portal_data`-Pfade in
`infrastructure.md` §3.7 sind mit diesem Stand obsolet.

## Konfiguration

`/etc/zukka/backup.env` (0600 root:root), Vorlage `env/backup.env.example`:

```sh
RESTIC_REPOSITORY=rclone:sbox01:/home/Backups/restic-planet-zukka
RESTIC_PASSWORD_FILE=/etc/zukka/restic.pw
```

Passwort einmalig erzeugen und **offline** verwahren (ohne es ist kein Restore
möglich):

```sh
openssl rand -hex 32 > /etc/zukka/restic.pw
chmod 0600 /etc/zukka/restic.pw
```

## Host-Installation (einmalig)

```sh
install -m 0755 /opt/apps/radio-zukka/repo/deploy/systemd/zukka-backup /usr/local/bin/zukka-backup
install -m 0644 /opt/apps/radio-zukka/repo/deploy/systemd/zukka-backup.service \
                /etc/systemd/system/zukka-backup.service
# … analog für zukka-backup.timer, zukka-backup-prune.{service,timer},
#     zukka-restore-drill.{service,timer}
systemctl daemon-reload
systemctl enable --now zukka-backup.timer zukka-backup-prune.timer zukka-restore-drill.timer
```

`rclone`-Remote `sbox01` muss als root konfiguriert sein (wie für das generische
planet-Backup). Prüfen:

```sh
systemctl list-timers 'zukka-*'
restic --repo "$RESTIC_REPOSITORY" --password-file /etc/zukka/restic.pw snapshots
```

## Kommandos

```sh
zukka-backup backup            # DB + Staging + Env + .deployed  (systemd: 04:15 Berlin)
zukka-backup predeploy <sha>   # nur DB, Tag predeploy-<sha>     (Deploy-Wrapper)
zukka-backup drill             # Restore + integrity_check + RTO (systemd: So 05:30)
zukka-backup prune             # forget 14d/8w/6m + prune       (systemd: So 05:00)
zukka-backup restore <dir>     # Notfall: neuester Snapshot -> <dir>
zukka-backup snapshots         # Snapshots auflisten
```

Der Staging-Baum liegt unter `/var/tmp/zukka-backup-stage/<run>/` und wird als
**relative** Pfade gesichert (`data/zukka.db`, `env/*.env`, `deploy.sha`,
`staging/…`), sodass ein Restore-Ziel selbsterklärend ist.

## Pre-Deploy-Snapshot (#43 im Deploy-Pfad)

`zukka-deploy prod --tag <sha>` ruft **vor** dem Image-`pull`
`zukka-backup predeploy <sha>`. Schlägt das fehl, bricht der Deploy ab
(fail-closed). Break-Glass nur im Notfall: `ZUKKA_SKIP_PREDEPLOY_BACKUP=1`
(loggt eine laute Warnung). Beim ersten Deploy auf leerem Host existiert noch
keine DB → der Pre-Deploy-Snapshot ist ein dokumentierter No-op (Exit 0).

## Notfall-Restore (< 30 min)

Ziel: eine bekannte, gute DB zurück auf das `zukka_data`-Volume.

```sh
# 1. Stack anhalten (schreibt keine neue DB mehr).
cd /opt/apps/radio-zukka/repo
docker compose -f compose/prod.yml --env-file /etc/zukka/prod.env stop scheduler portal tapes

# 2. Neuesten Snapshot restaurieren.
zukka-backup restore /var/tmp/restore-zukka

# 3. DB zurück ins Volume legen (Owner/Modus wie der Scheduler sie erwartet).
vol=/var/lib/docker/volumes/zukka-prod_zukka_data/_data
install -m 0600 /var/tmp/restore-zukka/data/zukka.db "$vol/zukka.db"
#    SQLite-WAL/Reste entfernen, sonst startet die alte WAL-Seite mit.
rm -f "$vol"/zukka.db-wal "$vol"/zukka.db-shm

# 4. Stack starten und Erreichbarkeit prüfen.
docker compose -f compose/prod.yml --env-file /etc/zukka/prod.env up -d --wait scheduler portal tapes
curl -fsS http://127.0.0.1:12010/readyz

# 5. Aufräumen.
rm -rf /var/tmp/restore-zukka
```

Erwartete Dauer: wenige Minuten (DB-Größe im MB-Bereich) — deutlich unter dem
30-min-RTO. Für einen älteren Stand statt `latest` die Snapshot-ID wählen:

```sh
zukka-backup snapshots
restic --repo "$RESTIC_REPOSITORY" --password-file /etc/zukka/restic.pw \
    restore <snapshot-id> --target /var/tmp/restore-zukka
```

## Restore-Drill

`zukka-backup drill` restauriert den jüngsten `zukka`-Snapshot nach
`/var/tmp/zukka-drill`, führt `PRAGMA integrity_check` aus, zählt die Tabellen
und prüft die Restore-Zeit gegen `ZUKKA_BACKUP_RTO_SECONDS` (Default 1800 s).
Jeder Fehlschlag → Exit ≠ 0 → systemd-Unit rot → Alertmanager `NodeSystemd`
bzw. Telegram (§3.9). Danach wird das Drill-Verzeichnis wieder gelöscht.

## Verifikation in CI

Der CI-Job `backup` führt den kompletten Pfad gegen ein **lokales** restic-Repo
aus: `backup` → `drill` → `prune`, plus Negativtests (leeres Repo → Drill rot;
korrupte DB → Drill rot). Damit sind Skript und Drill bei jeder PR grün/rot
nachweisbar, ohne PROD anzufassen.

## Troubleshooting

- **`rclone: command not found` / Auth-Fehler** — `sbox01`-Remote als root
  prüfen (`rclone lsd sbox01:`).
- **`wrong password`** — `RESTIC_PASSWORD_FILE` zeigt nicht auf das originale
  `/etc/zukka/restic.pw`. Ohne Passwort ist kein Restore möglich.
- **Drill rot, aber Backup grün** — `restic snapshots --tag zukka` und
  `sqlite3 <restored>/data/zukka.db "PRAGMA integrity_check;"` manuell prüfen.
- **Deploy bricht mit „pre-deploy backup failed"** — `zukka-backup predeploy
  <sha>` manuell laufen lassen und den restic-Fehler beheben; nicht mit
  `ZUKKA_SKIP_PREDEPLOY_BACKUP=1` dauerhaft umgehen.
