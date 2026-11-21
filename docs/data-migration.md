# Datenmigration — Legacy-Import (Issue #46)

Dieses Dokument beschreibt den einmaligen Import der Legacy-Daten (AzuraCast +
Funkwhale) in die bestehende `zukka.db`. Der Migrator liest **ausschließlich
einen dokumentierten JSON-Export**; es gibt **keinen Live-Zugriff** auf das
Legacy-System aus CI oder Produktion.

Abgrenzung: #46 ist ein eigenes Issue und **nicht** Teil des PROD-Cutovers
(siehe `cutover.md`). Das Schema selbst bleibt unverändert — es gibt **keine
neue Migration** und `scheduler/migrations/*` bleibt append-only.

Siehe auch `consolidated-architecture.md` §3 (Datenmodell) und §5.9 (Backup).

## 1. Werkzeug

`zukka-migrate` ist ein Binary des Scheduler-Crates
(`scheduler/src/bin/zukka-migrate.rs`, Logik in `scheduler/src/migrate/`).

```sh
# Dry-Run (Default): nur validieren und zählen, nie schreiben
zukka-migrate --legacy export.json --db /data/zukka.db

# Import: Backup, eine Transaktion, idempotent
zukka-migrate --legacy export.json --db /data/zukka.db --apply
```

`--dry-run` ist der Default; `--apply` schaltet das Schreiben frei.
`--dry-run` und `--apply` schließen sich gegenseitig aus.

### Exit-Codes

| Code | Bedeutung                                                     |
| ---- | ------------------------------------------------------------- |
| `0`  | Erfolg (Dry-Run ok oder Apply erfolgreich)                    |
| `1`  | Validierungs-/Waisen-/Duplikat-Fehler (kein Write)            |
| `2`  | I/O- oder Datenbank-Fehler                                    |

### Report

Jede Entität meldet `read`, `written` (Dry-Run: *würde schreiben*),
`skipped` (bereits vorhanden und unverändert) und `failed`. Es gilt die
Invariante `read == written + skipped + failed`; eine Verletzung ist ein Fehler
und wird vor jedem Write erkannt.

## 2. Exportformat (JSON)

Alle Zeitstempel sind **RFC3339-UTC** (`…T19:00:00.000Z`). Der Alt-Code rechnete
Berlin hart mit +2 h (CEST); der Export liefert bereits korrekte UTC-Instants,
der Migrator rechnet **nicht** nach (keine +2-Übernahme).

```json
{
  "version": 1,
  "generated_at": "2026-11-20T10:00:00.000Z",
  "casters": [
    { "username": "momo", "display_name": "momo",
      "email": "momo@radio-zukka.example", "aktiv": true }
  ],
  "tracks": [
    { "file_id": 1001, "title": "ANKD — 01.01.24 20:00", "artist": "ANKD",
      "album": "ANKD @ radio zukka", "duration_ms": 183000,
      "storage_key": "azura/1001.ogg", "isrc": "DEZZ00000001", "caster": "hovio" }
  ],
  "tapes": [
    { "legacy_id": "rec-2024-01-01-ankd", "broadcast_id": "bc-2024-01-01-ankd",
      "caster": "hovio", "title": "ANKD Live",
      "started_at": "2024-01-01T19:00:00.000Z",
      "ended_at": "2024-01-01T21:00:00.000Z",
      "storage_key": "recordings/rec-2024-01-01-ankd.ogg",
      "format": "ogg", "size_bytes": 123456789, "music_api_id": null }
  ]
}
```

Feldherkunft: `casters` ← Funkwhale-/AzuraCast-User, `tracks` ← AzuraCast-Library
(file id, Metadaten, Pfad, ISRC), `tapes` ← Aufnahme-Watcher (Recording/Broadcast
id, Streamer, Zeitraum, Storage-Key, Format, Größe).

## 3. Mapping je Entität

### Caster → `casters`

| Quelle            | Ziel                                                   |
| ----------------- | ------------------------------------------------------ |
| `username`        | `username` (lowercase, NOCASE-Naturschlüssel)          |
| `display_name`    | `display_name` (inkl. `DISPLAY_NAMES`-Mapping, s. u.)  |
| `email`           | `email` (leer → `NULL`)                                |
| `aktiv`           | `is_active`                                            |
| —                 | `role_id` = caster-Rolle `0192f0a1-0001-7000-8000-000000000003` |
| —                 | `password_hash` = **gesperrter** Argon2id-Hash         |
| —                 | `stream_username`/`stream_password_hash`/`harbor_mount` = `NULL` |

`DISPLAY_NAMES` (Legacy-Username → Anzeigename): `hovio → ANKD`,
`mctoastus → duck`, `momo → momo`. Ohne Mapping gewinnt der Legacy-Wert,
sonst der Username.

**Gesperrter Hash:** `casters.password_hash` ist der Argon2id-PHC-Hash eines
zufälligen Wegwerf-Secrets, das nirgends gespeichert wird. Ein Login mit
irgendeinem Passwort ist damit unmöglich; jeder migrierte Caster **muss ein
Passwort-Reset durchlaufen** (siehe `portal.md` §3.6). Der Wert ist eine
Konstante (`LOCKED_PASSWORD_HASH` in `migrate/map.rs`), damit Re-Läufe
byte-identisch und idempotent sind.

### Track → `tracks`

| Quelle        | Ziel                                                     |
| ------------- | -------------------------------------------------------- |
| `file_id`     | `music_api_id` = stringifizierte Legacy-File-ID (UNIQUE) |
| `title`/`artist`/`album` | gleichnamige Spalten (Default `''`)          |
| `duration_ms` | `duration_ms` (muss `>= 0` sein)                         |
| `storage_key` | `storage_key` (Pflicht, nicht leer)                      |
| `isrc`        | `isrc` **nur wenn nicht leer**, sonst `NULL`             |
| `caster`      | `caster_id` (aufgelöst; unbekannt → Waise)               |
| —             | `visibility = 'shared'`                                  |

Leere ISRC werden auf `NULL` abgebildet, um UNIQUE-Kollisionen durch leere
Strings zu vermeiden.

### Tape → `tapes`

| Quelle         | Ziel                                                        |
| -------------- | ----------------------------------------------------------- |
| `broadcast_id`/`legacy_id` | `slug` = 8 Zeichen Crockford-Base32 aus SHA-256(Naturschlüssel) |
| `caster`       | `caster_id` (Pflicht; nicht auflösbar → Waise)              |
| `title`        | `title`                                                     |
| `started_at`/`ended_at` | UTC-RFC3339-ms; `ended_at >= started_at` erzwungen |
| —              | `duration_ms` = Differenz                                   |
| `broadcast_id` vorhanden | `source = 'live'`, sonst `source = 'upload'`      |
| —              | `status = 'ready'`, `visible = 0`, `published_at = NULL`    |
| `music_api_id`/`format`/`size_bytes` | gleichnamige Spalten (`format` ∈ `ogg`, `mp3`) |

Der Slug ist deterministisch und stabil über Läufe; er ist der Naturschlüssel
des Upserts und damit die Grundlage der Idempotenz.

## 4. Dry-Run vs. Apply

- **Dry-Run (Default):** öffnet die DB **read-only**, führt **kein**
  INSERT/UPDATE aus, zählt und listet Probleme. Bei Waisen/Duplikaten bricht er
  mit vollständiger Liste und Exit `1` ab.
- **Apply:** erstellt zuerst ein Backup, importiert dann **in einer einzigen
  Transaktion** (`BEGIN … COMMIT`, Rollback bei Fehler) in FK-Reihenfolge
  **Casters → Tracks → Tapes**. Upserts laufen über die Naturschlüssel
  (`casters.username`, `tracks.music_api_id`, `tapes.slug`): ein zweiter Lauf
  schreibt **0 neu** und ändert 0 Zeilen. Nach dem Import (vor dem Commit) läuft
  `PRAGMA foreign_key_check`; nicht leer → Rollback und Exit `1`.

## 5. Waisen & Duplikate

Vor jedem Write sammelt der Migrator vollständige Problemlisten (Entität +
Legacy-Key + Grund):

1. **Tape ohne auflösbaren Caster** (Waisen-Tape),
2. **Track mit unbekanntem Caster**,
3. **doppelte Naturschlüssel** (`casters.username`, `tracks.music_api_id`/`isrc`,
   `tapes.slug`/`music_api_id`),
4. Feldverstöße (leerer `storage_key`, `duration_ms < 0`, `ended_at < started_at`,
   unbekanntes `format`).

Bei mindestens einem Problem wird **nichts** geschrieben (auch im Apply-Modus,
vor dem Backup) und Exit `1` geliefert.

## 6. Backup & Rollback

- **Backup:** vor jedem Apply `VACUUM INTO '<db>.bak'` (atomar, WAL-sicher).
  Ein vorhandenes `.bak` wird **nicht** überschrieben → Fehler (Exit `2`).
- **Rollback:** Migrations-Binary stoppen, `zukka.db` durch `<db>.bak` ersetzen,
  Binary neu starten. Da die Migration rein app-seitig importiert (keine
  Schemaänderung), ist der Rückweg ein reiner Dateiaustausch.
- Die SQL-Migrationen (`scheduler/migrations/*`) bleiben **append-only** und
  werden von #46 nicht verändert.

```sh
# Rollback
systemctl stop zukka-scheduler
cp /data/zukka.db.bak /data/zukka.db
systemctl start zukka-scheduler
```

## 7. Tests

`scheduler/tests/data_migration_*.rs` (Fixture-Export + temporäre DB):

| Test                        | Beweist                                           |
| --------------------------- | ------------------------------------------------- |
| `data_migration_dry_run`    | Dry-Run schreibt nichts, kein Backup, Invariante  |
| `data_migration_apply`      | Zählung = Fixture, Slug/Hash/Enum-Assertions      |
| `data_migration_orphans`    | Waisen/Duplikate → Abbruch mit Liste, kein Write  |
| `data_migration_idempotency`| 2. Lauf schreibt 0 neu, IDs stabil                |
| `data_migration_fk`         | `foreign_key_check` leer, keine Waisen nach Apply |

```sh
export CARGO_TARGET_DIR=/srv/zukka-build-46/target
cargo test -p zukka-scheduler --test 'data_migration_*' --locked
```
