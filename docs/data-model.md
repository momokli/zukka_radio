# Datenmodell & API

## 1. Scope

Dieses Dokument beschreibt das Datenmodell und die API-Contracts für den neuen radio-zukka-Stack auf Branch `v2`. Es ist die verbindliche Grundlage für die beiden Rust-Services `scheduler` und `portal`.

**Im Scope:**

- SQLite-Schema inklusive Tabellen, Spalten, Typen, Indizes und Constraints für:
  `casters`, `roles`, `shows`, `broadcasts`, `sources`, `queues`, `queue_items`, `tracks`, `tapes`, `announcements`, `announcement_templates`, `schedule_log`, `sessions`.
- API-Contracts für den internen `scheduler` und das öffentliche `portal`.
- Typen und Enums: `BroadcastKind`, `SourceKind`, `Priority`, `Role` sowie die zusätzlich benötigten Enums.
- Zeitfelder und DST-Verhalten mit `Europe/Berlin` und `chrono-tz`.
- Migrationsstrategie mit `sqlx`.

**Nicht im Scope:**

- Liquidsoap-Skriptdetails, Caddy/Infrastruktur, Deployment-Runbook.
- UI-Design und Frontend-Komponenten.
- Interne API der Music API. Es wird nur die Integration aus Sicht von `scheduler` und `portal` beschrieben.
- Konkrete Telegram-Bot-Implementierung; nur das Datenmodell und die API-Verträge für Announcements.

**Annahmen:**

- Es gibt zwei getrennte SQLite-Datenbanken: `scheduler.db` und `portal.db`.
- Der `scheduler` ist die Schreibautorität für alle Domänendaten. Das `portal` schreibt ausschließlich Sessions und ruft für Domänendaten die Scheduler-API auf.
- Die Music API ist die Autorität für Dateien und Objekt-Storage. `tracks` und `tapes` speichern lokale Referenzen und Metadaten.
- Alle Zeitfenster, Schedule-Logs und Announcements werden fachlich in `Europe/Berlin` geführt. Die DB speichert Instants als UTC, die API liefert Berliner Zeit mit Offset.
- Dieses Dokument beschreibt ausschließlich die neue Welt. Es gibt keine Kompatibilitäts- oder Migrationsanforderungen zu früheren Systemen.

**Begriffe:**

| Begriff      | Bedeutung                                                                                                            |
| ------------ | -------------------------------------------------------------------------------------------------------------------- |
| Caster       | Portal-User plus Streamer-Credentials.                                                                               |
| Show         | Optionales Format mit Name, Beschreibung, Artwork, Links und Default-Schedule.                                       |
| Broadcast    | Slot mit Inhalt: `live`, `recording` oder `playlist`.                                                                |
| Source       | Prioritäts-Layer, der aktuell oder potenziell ausgegeben wird.                                                       |
| Queue        | Benannte Sammlung von Tracks/Tapes, pro Caster. `background` läuft kontinuierlich, `slot` gehört zu einem Broadcast. |
| Tape         | Live mitgeschnittene oder hochgeladene Aufnahme. Beides ist dasselbe Objekt.                                         |
| Announcement | Geplante oder manuelle Nachricht, aktuell Kanal Telegram.                                                            |

## 2. Entscheidungen

### 2.1 Architektur- und DB-Aufteilung

| Entscheidung                                                               | Begründung                                                                                   |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Zwei SQLite-DBs: `scheduler.db` und `portal.db`.                           | Klare Service-Grenzen, ein Writer pro DB, keine Cross-Service-Locks.                         |
| `scheduler.db` enthält alle Domänendaten.                                  | Der Scheduler ist die Autorität für Queue, Rotation, Schedule, Prioritäten und Live-Auswahl. |
| `portal.db` enthält nur `sessions`.                                        | Das Portal ist die Auth-Autorität für Browser-Sessions. Es schreibt keine Domänendaten.      |
| Das Portal ruft für Domänendaten ausschließlich die Scheduler-API auf.     | Keine geteilte DB, keine versteckten Kopplungen.                                             |
| Der Scheduler stellt eine interne Auth-Verifikation für das Portal bereit. | Casters und Rollen liegen im Scheduler; das Portal speichert nur die Session.                |

### 2.2 IDs

| Entscheidung                                                                                                                    | Begründung                                                                                                                         |
| ------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| Interne IDs sind `INTEGER PRIMARY KEY` (SQLite rowid); `casters`, `broadcasts` und `sources` nutzen zusätzlich `AUTOINCREMENT`. | Einfach, schnell, gute FK-Performance. `AUTOINCREMENT` verhindert die Wiederverwendung von IDs, auf die `schedule_log` verweist.   |
| Externe Music-API-IDs werden separat gespeichert (`music_api_id`).                                                              | Keine Vermischung von lokaler und externer Identität.                                                                              |
| Session-Tokens sind opake 256-Bit-Zufallswerte; in der DB liegt nur der SHA-256-Hash.                                           | Kein Klartext-Token in DB, Logs oder Backups.                                                                                      |
| Keine UUIDs/ULIDs in v1.                                                                                                        | Für eine Single-Node-SQLite unnötige Komplexität. Bei späterer Verteilung kann eine zusätzliche `public_id`-Spalte ergänzt werden. |

### 2.3 Zeitmodell

| Entscheidung                                                                                                                                               | Begründung                                                                                                          |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Instants werden in SQLite als `TEXT` im Format RFC3339 UTC gespeichert (`YYYY-MM-DDTHH:MM:SS.sssZ`).                                                       | Eindeutig, DST-sicher, lexikografisch sortierbar.                                                                   |
| Die API akzeptiert und liefert Zeitfelder in `Europe/Berlin` mit Offset (`+01:00`/`+02:00`).                                                               | Fachliche Anforderung. Offsets sind eindeutig und DST-korrekt.                                                      |
| Wiederkehrende Regeln (`default_schedule`) speichern lokale Wandzeit plus `timezone: "Europe/Berlin"`.                                                     | Eine Wandzeit ist keine Instant; sie muss pro Vorkommen in einen Instant aufgelöst werden.                          |
| `chrono-tz` ist die einzige Quelle für Zeitzonenlogik.                                                                                                     | Keine hartcodierten UTC-Offsets, keine eigene DST-Logik.                                                            |
| DST-Policy: nicht existierende lokale Zeit wird nach vorne verschoben; mehrdeutige Startzeit nimmt das frühere Vorkommen, mehrdeutige Endzeit das spätere. | Verhindert Null- oder Negativdauern und stille Verschiebungen. Jede Anpassung wird in `schedule_log` protokolliert. |

### 2.4 Enums und Constraints

| Entscheidung                                                                      | Begründung                                                             |
| --------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Enums werden als `TEXT` mit `CHECK`-Constraint gespeichert.                       | Lesbar, portabel, keine SQLite-Custom-Types.                           |
| Rust nutzt `sqlx::Type` + `serde` mit `snake_case`.                               | DB, JSON und Rust-Repräsentation sind identisch.                       |
| `Priority` ist ein Newtype `i32` mit Default-Konstanten, kein geschlossenes Enum. | Die Architektur verlangt „Defaults, später pro Source konfigurierbar“. |
| `Role` ist ein Enum mit den Werten `admin`, `operator`, `caster`.                 | Die Rollenmatrix ist noch nicht final; die drei Rollen sind stabil.    |

### 2.5 RBAC

| Entscheidung                                                                                               | Begründung                                                                        |
| ---------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `roles` ist eine Tabelle mit JSON-Permissions.                                                             | Erlaubt eine permissive Startmatrix und spätere Verfeinerung ohne Schemaänderung. |
| Built-in-Rollen: `admin`, `operator`, `caster`.                                                            | Deckt die in der Architektur genannten Fälle ab.                                  |
| Permissiv by default: Casters dürfen Shows anlegen, Broadcasts planen, Library und eigene Queue verwalten. | Entspricht der Vision „Jeder darf Shows anlegen und Broadcasts planen“.           |
| Admin/Operator-Aktionen werden zusätzlich im `schedule_log` protokolliert.                                 | Auditierbarkeit bei permissiven Defaults.                                         |

### 2.6 Broadcasts, Queues, Tapes

| Entscheidung                                                                                       | Begründung                                                                   |
| -------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `broadcasts.kind` ist `live`, `recording` oder `playlist`.                                         | Entspricht der Architektur.                                                  |
| `recording` referenziert genau ein `tape`; `playlist` genau eine `queue`; `live` keins von beiden. | Eindeutige Content-Zuordnung per CHECK.                                      |
| `queues.kind` ist `background` oder `slot`.                                                        | `background` läuft kontinuierlich; `slot` ist die Playlist eines Broadcasts. |
| `queue_items` referenzieren genau entweder `track` oder `tape`.                                    | Beide sind abspielbar; CHECK verhindert Mischzustände.                       |
| `tapes` ist eine Tabelle für Live-Recordings und Uploads.                                          | Die Architektur sagt explizit „dasselbe“.                                    |
| `tapes.broadcast_id` ist optionale Provenienz.                                                     | Ein Tape kann aus einem Broadcast stammen, muss aber nicht.                  |

### 2.7 Announcements

| Entscheidung                                                                     | Begründung                                                                         |
| -------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `announcement_templates` global (`show_id IS NULL`) oder pro Show.               | Globale Defaults plus Show-Override.                                               |
| Partielle Unique-Indizes erzwingen genau ein Template pro Event/Kanal und Ebene. | Verhindert versehentliche Duplikate.                                               |
| `announcements.dedupe_key` ist optional und unique.                              | Idempotente Events (z. B. `live_on:<broadcast_id>`) werden nicht doppelt gesendet. |
| `attempts`, `next_attempt_at`, `error` sind im Schema.                           | Retry und Fehleranalyse ohne zusätzliche Tabelle.                                  |

### 2.8 Schedule-Log

| Entscheidung                                                            | Begründung                                                                        |
| ----------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `schedule_log` ist append-only.                                         | Audit, Debugging und Nachvollziehbarkeit von Prioritäts- und Live-Entscheidungen. |
| Trigger verhindern `UPDATE` und `DELETE`.                               | Schutz vor versehentlichen Änderungen.                                            |
| Retention erfolgt über Archivierung, nicht über Löschen in der Tabelle. | Append-only bleibt intakt.                                                        |

### 2.9 Sessions

| Entscheidung                                                    | Begründung                                                                  |
| --------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `sessions` liegt in `portal.db`.                                | Das Portal ist die Auth-Autorität.                                          |
| `caster_id` ist ein logischer FK auf `scheduler.db.casters.id`. | Kein Cross-DB-FK in SQLite; Integrität wird über die Scheduler-API geprüft. |
| Session-Token wird nur gehasht gespeichert.                     | Sicherheit bei DB-Leak oder Backup-Zugriff.                                 |
| Expiry und Revocation sind explizit modelliert.                 | Kein stilles „Session gilt ewig“.                                           |

### 2.10 API und Auth

| Entscheidung                                                                               | Begründung                                                            |
| ------------------------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| Beide APIs sind unter `/api/v1` versioniert.                                               | Spätere Breaking Changes bleiben möglich.                             |
| JSON, RFC3339, einheitlicher Error-Envelope.                                               | Konsistenz für Portal, Scheduler und Liquidsoap.                      |
| Scheduler-API ist intern; Portal-API ist öffentlich.                                       | Klare Angriffsfläche.                                                 |
| Portal → Scheduler mit Service-Token plus Caster-Kontext (`X-Caster-Id`, `X-Caster-Role`). | Der Scheduler vertraut dem Portal, prüft aber zusätzlich Permissions. |
| Liquidsoap → Scheduler mit separatem Token.                                                | Kein Caster-Kontext nötig; nur Playback- und Control-Endpunkte.       |

### 2.11 Migrations

| Entscheidung                                                       | Begründung                                                                    |
| ------------------------------------------------------------------ | ----------------------------------------------------------------------------- |
| `sqlx::migrate!` mit append-only Migrationen.                      | Reproduzierbar, CI-fähig, kein manuelles Schema-Driften.                      |
| WAL, `foreign_keys=ON`, `busy_timeout=5000`, `synchronous=NORMAL`. | SQLite für einen Single-Writer-Dienst robust konfigurieren.                   |
| `STRICT`-Tabellen.                                                 | Frühe Typfehler statt stiller Konvertierungen.                                |
| Backups vor jeder Migration.                                       | SQLite-Änderungen sind zwar transaktional, aber nicht unbegrenzt rückrollbar. |

## 3. Schnittstellen (Schema, Endpunkte, Typen)

### 3.1 SQLite-Schema

#### 3.1.1 Scheduler-DB

Voraussetzungen: SQLite >= 3.38 (für `STRICT` und JSON1). Alle Tabellen sind `STRICT`. Die DDL läuft in einer sqlx-Migration.

```sql
-- scheduler.db
-- Connection-Setup (nicht in der Migration):
--   journal_mode=WAL
--   foreign_keys=ON
--   busy_timeout=5000
--   synchronous=NORMAL

-- 1. Rollen
CREATE TABLE roles (
    id            INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL UNIQUE
                          CHECK (name IN ('admin', 'operator', 'caster')),
    description   TEXT    NOT NULL DEFAULT '',
    permissions   TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(permissions)),
    is_system     INTEGER NOT NULL DEFAULT 1 CHECK (is_system IN (0, 1)),
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 2. Caster (Portal-User + Streamer-Credentials)
CREATE TABLE casters (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    username             TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    display_name         TEXT    NOT NULL,
    email                TEXT    COLLATE NOCASE UNIQUE,
    password_hash        TEXT    NOT NULL,
    role_id              INTEGER NOT NULL REFERENCES roles(id) ON DELETE RESTRICT,
    stream_username      TEXT    COLLATE NOCASE UNIQUE,
    stream_password_hash TEXT,
    harbor_mount         TEXT    UNIQUE,
    avatar_url           TEXT,
    bio                  TEXT    NOT NULL DEFAULT '',
    is_active            INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at           TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at           TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 3. Shows
CREATE TABLE shows (
    id               INTEGER PRIMARY KEY,
    slug             TEXT    NOT NULL UNIQUE,
    name             TEXT    NOT NULL,
    description      TEXT    NOT NULL DEFAULT '',
    artwork_url      TEXT,
    links            TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(links)),
    default_schedule TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(default_schedule)),
    owner_caster_id  INTEGER REFERENCES casters(id) ON DELETE SET NULL,
    created_by       INTEGER NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    created_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 4. Queues
CREATE TABLE queues (
    id          INTEGER PRIMARY KEY,
    caster_id   INTEGER NOT NULL REFERENCES casters(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    kind        TEXT    NOT NULL DEFAULT 'background'
                        CHECK (kind IN ('background', 'slot')),
    is_active   INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    version     INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (caster_id, name)
) STRICT;

-- 5. Tracks (lokale Referenz auf Music API)
CREATE TABLE tracks (
    id              INTEGER PRIMARY KEY,
    music_api_id    TEXT    NOT NULL UNIQUE,
    isrc            TEXT    UNIQUE,
    title           TEXT    NOT NULL,
    artist          TEXT    NOT NULL DEFAULT '',
    album           TEXT    NOT NULL DEFAULT '',
    duration_ms     INTEGER NOT NULL CHECK (duration_ms >= 0),
    storage_key     TEXT    NOT NULL,
    artwork_url     TEXT,
    visibility      TEXT    NOT NULL DEFAULT 'shared'
                            CHECK (visibility IN ('shared', 'private')),
    owner_caster_id INTEGER REFERENCES casters(id) ON DELETE SET NULL,
    metadata        TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 6. Tapes (Live-Recording oder Upload — dasselbe)
CREATE TABLE tapes (
    id            INTEGER PRIMARY KEY,
    title         TEXT    NOT NULL,
    description   TEXT    NOT NULL DEFAULT '',
    caster_id     INTEGER NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    show_id       INTEGER REFERENCES shows(id) ON DELETE SET NULL,
    broadcast_id  INTEGER REFERENCES broadcasts(id) ON DELETE SET NULL
                          DEFERRABLE INITIALLY DEFERRED,
    music_api_id  TEXT    UNIQUE,
    storage_key   TEXT,
    duration_ms   INTEGER NOT NULL CHECK (duration_ms >= 0),
    recorded_at   TEXT    NOT NULL,
    published_at  TEXT,
    status        TEXT    NOT NULL DEFAULT 'draft'
                          CHECK (status IN ('draft', 'published', 'archived')),
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 7. Broadcasts
CREATE TABLE broadcasts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    show_id     INTEGER REFERENCES shows(id) ON DELETE SET NULL,
    caster_id   INTEGER NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    kind        TEXT    NOT NULL CHECK (kind IN ('live', 'recording', 'playlist')),
    title       TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    starts_at   TEXT    NOT NULL,
    ends_at     TEXT    NOT NULL,
    status      TEXT    NOT NULL DEFAULT 'scheduled'
                        CHECK (status IN ('scheduled', 'live', 'ended', 'cancelled')),
    tape_id     INTEGER REFERENCES tapes(id) ON DELETE SET NULL
                        DEFERRABLE INITIALLY DEFERRED,
    queue_id    INTEGER REFERENCES queues(id) ON DELETE SET NULL,
    version     INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by  INTEGER NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (ends_at > starts_at),
    CHECK (
        (kind = 'live'      AND tape_id IS NULL     AND queue_id IS NULL) OR
        (kind = 'recording' AND tape_id IS NOT NULL AND queue_id IS NULL) OR
        (kind = 'playlist'  AND tape_id IS NULL     AND queue_id IS NOT NULL)
    )
) STRICT;

-- 8. Sources (Prioritäts-Layer)
CREATE TABLE sources (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL UNIQUE,
    kind        TEXT    NOT NULL
                        CHECK (kind IN ('live', 'show', 'queue', 'rotation', 'relay', 'fun')),
    priority    INTEGER NOT NULL CHECK (priority BETWEEN 1 AND 100),
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    caster_id   INTEGER REFERENCES casters(id) ON DELETE SET NULL,
    queue_id    INTEGER REFERENCES queues(id) ON DELETE SET NULL,
    show_id     INTEGER REFERENCES shows(id) ON DELETE SET NULL,
    config      TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(config)),
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 9. Queue-Items
CREATE TABLE queue_items (
    id             INTEGER PRIMARY KEY,
    queue_id       INTEGER NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
    position       INTEGER NOT NULL CHECK (position >= 0),
    track_id       INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    tape_id        INTEGER REFERENCES tapes(id) ON DELETE CASCADE,
    title_override TEXT,
    state          TEXT    NOT NULL DEFAULT 'pending'
                           CHECK (state IN ('pending', 'playing', 'played', 'skipped')),
    played_at      TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (queue_id, position),
    CHECK ((track_id IS NOT NULL) <> (tape_id IS NOT NULL))
) STRICT;

-- 10. Announcement-Templates
CREATE TABLE announcement_templates (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    event       TEXT    NOT NULL
                        CHECK (event IN ('live_on', 'live_off', 'show_reminder',
                                         'recording_published', 'error', 'fallback', 'manual')),
    show_id     INTEGER REFERENCES shows(id) ON DELETE CASCADE,
    channel     TEXT    NOT NULL DEFAULT 'telegram' CHECK (channel IN ('telegram')),
    body        TEXT    NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 11. Announcements
CREATE TABLE announcements (
    id              INTEGER PRIMARY KEY,
    event           TEXT    NOT NULL
                            CHECK (event IN ('live_on', 'live_off', 'show_reminder',
                                             'recording_published', 'error', 'fallback', 'manual')),
    show_id         INTEGER REFERENCES shows(id) ON DELETE SET NULL,
    broadcast_id    INTEGER REFERENCES broadcasts(id) ON DELETE SET NULL,
    caster_id       INTEGER REFERENCES casters(id) ON DELETE SET NULL,
    template_id     INTEGER REFERENCES announcement_templates(id) ON DELETE SET NULL,
    channel         TEXT    NOT NULL DEFAULT 'telegram' CHECK (channel IN ('telegram')),
    payload         TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
    dedupe_key      TEXT    UNIQUE,
    scheduled_at    TEXT,
    sent_at         TEXT,
    status          TEXT    NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending', 'sent', 'failed', 'cancelled')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TEXT,
    error           TEXT,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- 12. Schedule-Log (append-only)
CREATE TABLE schedule_log (
    id            INTEGER PRIMARY KEY,
    occurred_at   TEXT    NOT NULL,
    event         TEXT    NOT NULL
                          CHECK (event IN ('broadcast_created', 'broadcast_updated',
                                           'broadcast_cancelled', 'broadcast_started',
                                           'broadcast_ended', 'source_switched',
                                           'live_connected', 'live_disconnected',
                                           'fallback', 'error', 'dst_adjusted',
                                           'announcement_sent', 'announcement_failed')),
    -- Bewusst keine FKs: schedule_log ist append-only und muss Löschungen
    -- von Broadcasts/Sources/Casters überleben. Die Anwendung validiert die IDs.
    broadcast_id  INTEGER,
    source_id     INTEGER,
    caster_id     INTEGER,
    details       TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(details)),
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Indizes
CREATE INDEX idx_casters_role ON casters(role_id);
CREATE INDEX idx_casters_active ON casters(is_active);

CREATE INDEX idx_shows_owner ON shows(owner_caster_id);
CREATE INDEX idx_shows_created_by ON shows(created_by);

CREATE INDEX idx_queues_caster ON queues(caster_id);
CREATE INDEX idx_queues_kind ON queues(kind);

CREATE INDEX idx_tracks_artist ON tracks(artist);
CREATE INDEX idx_tracks_owner ON tracks(owner_caster_id);
CREATE INDEX idx_tracks_visibility ON tracks(visibility);

CREATE INDEX idx_tapes_caster ON tapes(caster_id);
CREATE INDEX idx_tapes_show ON tapes(show_id);
CREATE INDEX idx_tapes_broadcast ON tapes(broadcast_id);
CREATE INDEX idx_tapes_status ON tapes(status);
CREATE INDEX idx_tapes_published ON tapes(published_at);

CREATE INDEX idx_broadcasts_starts_at ON broadcasts(starts_at);
CREATE INDEX idx_broadcasts_ends_at ON broadcasts(ends_at);
CREATE INDEX idx_broadcasts_show ON broadcasts(show_id);
CREATE INDEX idx_broadcasts_caster ON broadcasts(caster_id);
CREATE INDEX idx_broadcasts_status ON broadcasts(status);
CREATE INDEX idx_broadcasts_kind ON broadcasts(kind);

CREATE UNIQUE INDEX uq_broadcasts_show_start
    ON broadcasts(show_id, starts_at)
    WHERE show_id IS NOT NULL;

CREATE UNIQUE INDEX uq_broadcasts_show_start
    ON broadcasts(show_id, starts_at)
    WHERE show_id IS NOT NULL;

CREATE INDEX idx_sources_priority ON sources(priority DESC);
CREATE INDEX idx_sources_kind ON sources(kind);
CREATE INDEX idx_sources_enabled ON sources(enabled);

CREATE INDEX idx_queue_items_queue_position ON queue_items(queue_id, position);
CREATE INDEX idx_queue_items_track ON queue_items(track_id);
CREATE INDEX idx_queue_items_tape ON queue_items(tape_id);
CREATE INDEX idx_queue_items_state ON queue_items(state);

CREATE UNIQUE INDEX uq_announcement_templates_global
    ON announcement_templates(event, channel)
    WHERE show_id IS NULL;

CREATE UNIQUE INDEX uq_announcement_templates_show
    ON announcement_templates(event, show_id, channel)
    WHERE show_id IS NOT NULL;

CREATE INDEX idx_announcement_templates_show ON announcement_templates(show_id);

CREATE INDEX idx_announcements_status_scheduled ON announcements(status, scheduled_at);
CREATE INDEX idx_announcements_show ON announcements(show_id);
CREATE INDEX idx_announcements_broadcast ON announcements(broadcast_id);
CREATE INDEX idx_announcements_caster ON announcements(caster_id);

CREATE INDEX idx_schedule_log_occurred ON schedule_log(occurred_at);
CREATE INDEX idx_schedule_log_event ON schedule_log(event);
CREATE INDEX idx_schedule_log_broadcast ON schedule_log(broadcast_id);
CREATE INDEX idx_schedule_log_source ON schedule_log(source_id);
CREATE INDEX idx_schedule_log_caster ON schedule_log(caster_id);

-- Append-only-Schutz
CREATE TRIGGER trg_schedule_log_no_update
BEFORE UPDATE ON schedule_log
BEGIN
    SELECT RAISE(ABORT, 'schedule_log is append-only');
END;

CREATE TRIGGER trg_schedule_log_no_delete
BEFORE DELETE ON schedule_log
BEGIN
    SELECT RAISE(ABORT, 'schedule_log is append-only');
END;
```

**Hinweise zu den Constraints:**

- `broadcasts.ends_at > starts_at` funktioniert, weil alle Instants im gleichen UTC-Format gespeichert werden. Die Anwendung muss das Format erzwingen; optional kann zusätzlich ein `GLOB`-CHECK ergänzt werden.
- `tapes.broadcast_id` und `broadcasts.tape_id` sind zirkulär. Beide FKs sind `DEFERRABLE INITIALLY DEFERRED`. Insert-Reihenfolge: Tape mit `broadcast_id = NULL` anlegen, Broadcast mit `tape_id` anlegen, danach `tapes.broadcast_id` setzen. Alternativ in einer Transaktion mit deferred Constraints.
- `sources` erlaubt generische Layer ohne FK (`live`, `rotation`, `relay`, `fun`) sowie spezifische Layer mit `queue_id`/`show_id`/`caster_id`. Die Verknüpfung ist optional; die Anwendung validiert die Semantik.
- `queue_items.position` ist pro Queue unique. Die API nutzt Positionen mit Lücken (z. B. 1000, 2000, 3000) und rebalanciert bei Bedarf in einer Transaktion.
- `announcements.dedupe_key` ist unique, aber nullable. Mehrere `NULL`-Werte sind erlaubt; gesetzte Keys verhindern Duplikate.
- `schedule_log` ist append-only. Retention erfolgt über ein Archiv, nicht über `DELETE`.
- `schedule_log` hat bewusst keine FKs. `ON DELETE SET NULL` würde ein `UPDATE` auf der append-only-Tabelle auslösen und vom Trigger abbrechen. Die Anwendung schreibt die IDs als historische Referenz; `AUTOINCREMENT` auf `casters`, `broadcasts` und `sources` verhindert ID-Wiederverwendung.
- Ein Tape, das von einem `recording`-Broadcast referenziert wird, kann nicht gelöscht werden: `ON DELETE SET NULL` würde den CHECK verletzen. Die API mappt das auf `409`. Gleiches gilt für `queue_id` bei `playlist`.
- Der partielle Unique-Index `uq_broadcasts_show_start` macht die Schedule-Expansion idempotent. `POST /broadcasts` mit demselben `(show_id, starts_at)` liefert `409`.
- Der partielle Unique-Index `uq_broadcasts_show_start` macht die Schedule-Expansion idempotent. `POST /broadcasts` mit demselben `(show_id, starts_at)` liefert `409`.

#### 3.1.2 Portal-DB

```sql
-- portal.db
-- Connection-Setup wie scheduler.db

CREATE TABLE sessions (
    id            INTEGER PRIMARY KEY,
    caster_id     INTEGER NOT NULL, -- logischer FK auf scheduler.db.casters.id
    token_hash    TEXT    NOT NULL UNIQUE,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at    TEXT    NOT NULL,
    last_seen_at  TEXT,
    revoked_at    TEXT,
    user_agent    TEXT,
    ip_address    TEXT,
    CHECK (expires_at > created_at)
) STRICT;

CREATE INDEX idx_sessions_caster ON sessions(caster_id);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
CREATE INDEX idx_sessions_revoked ON sessions(revoked_at);
```

**Hinweise:**

- `caster_id` hat bewusst keinen FK, weil `casters` in einer anderen DB liegt. Das Portal validiert die Caster-ID bei Login und bei jeder Session-Nutzung über die Scheduler-API.
- `token_hash` ist SHA-256 des opaken Tokens. Das Klartext-Token wird nur einmal an den Client ausgegeben.
- `expires_at` ist ein Instant. Die API liefert es in `Europe/Berlin`.

#### 3.1.3 Connection-Setup und Seed

**Connection-Setup (beide Services):**

```rust
let options = SqliteConnectOptions::new()
    .filename("scheduler.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .foreign_keys(true)
    .busy_timeout(Duration::from_secs(5))
    .synchronous(SqliteSynchronous::Normal);
```

**Seed-Daten (Scheduler-Migration):**

```sql
INSERT INTO roles (name, description, permissions, is_system) VALUES
  ('admin',    'Vollzugriff', '{"*": true}', 1),
  ('operator', 'Live-Auswahl, Caster-Verwaltung',
   '{"casters.manage": true, "live.select": true, "sources.manage": true,
     "announcements.post": true, "shows.create": true, "broadcasts.plan": true,
     "library.manage": true}', 1),
  ('caster',   'Standard-Caster',
   '{"shows.create": true, "broadcasts.plan": true, "library.manage": true,
     "announcements.post": true}', 1)
ON CONFLICT(name) DO NOTHING;

INSERT INTO sources (name, kind, priority, enabled, config) VALUES
  ('live',     'live',     100, 1, '{"harbor": "live", "mount_prefix": "/live"}'),
  ('show',     'show',      80, 1, '{}'),
  ('queue',    'queue',     60, 1, '{}'),
  ('rotation', 'rotation',  40, 1, '{}'),
  ('relay',    'relay',     20, 1, '{"url": "https://ice1.somafm.com/groovesalad-128-mp3"}'),
  ('fun',      'fun',        1, 1, '{}')
ON CONFLICT(name) DO NOTHING;
```

Die Seed-Sources sind generische Layer. Konkrete Queue-/Show-Sources werden später mit `queue_id`/`show_id` angelegt. Die Default-Prioritäten entsprechen der Architektur und sind pro Source änderbar.

### 3.2 Typen/Enums

| Typ                  | Werte                                                                                                                                                                                                                                              | SQL               | JSON         |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------- | ------------ |
| `BroadcastKind`      | `live`, `recording`, `playlist`                                                                                                                                                                                                                    | `TEXT` + CHECK    | `snake_case` |
| `SourceKind`         | `live`, `show`, `queue`, `rotation`, `relay`, `fun`                                                                                                                                                                                                | `TEXT` + CHECK    | `snake_case` |
| `Priority`           | `1..100`; Defaults: `live=100`, `show=80`, `queue=60`, `rotation=40`, `relay=20`, `fun=1`                                                                                                                                                          | `INTEGER` + CHECK | `integer`    |
| `Role`               | `admin`, `operator`, `caster`                                                                                                                                                                                                                      | `TEXT` + CHECK    | `snake_case` |
| `BroadcastStatus`    | `scheduled`, `live`, `ended`, `cancelled`                                                                                                                                                                                                          | `TEXT` + CHECK    | `snake_case` |
| `QueueKind`          | `background`, `slot`                                                                                                                                                                                                                               | `TEXT` + CHECK    | `snake_case` |
| `QueueItemState`     | `pending`, `playing`, `played`, `skipped`                                                                                                                                                                                                          | `TEXT` + CHECK    | `snake_case` |
| `TapeStatus`         | `draft`, `published`, `archived`                                                                                                                                                                                                                   | `TEXT` + CHECK    | `snake_case` |
| `AnnouncementEvent`  | `live_on`, `live_off`, `show_reminder`, `recording_published`, `error`, `fallback`, `manual`                                                                                                                                                       | `TEXT` + CHECK    | `snake_case` |
| `AnnouncementStatus` | `pending`, `sent`, `failed`, `cancelled`                                                                                                                                                                                                           | `TEXT` + CHECK    | `snake_case` |
| `ScheduleEvent`      | `broadcast_created`, `broadcast_updated`, `broadcast_cancelled`, `broadcast_started`, `broadcast_ended`, `source_switched`, `live_connected`, `live_disconnected`, `fallback`, `error`, `dst_adjusted`, `announcement_sent`, `announcement_failed` | `TEXT` + CHECK    | `snake_case` |
| `Visibility`         | `shared`, `private`                                                                                                                                                                                                                                | `TEXT` + CHECK    | `snake_case` |

**Rust-Definitionen (Auszug):**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BroadcastKind {
    Live,
    Recording,
    Playlist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Live,
    Show,
    Queue,
    Rotation,
    Relay,
    Fun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Operator,
    Caster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, sqlx::Type, serde::Serialize, serde::Deserialize)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct Priority(pub i32);

impl Priority {
    pub const LIVE: Self = Self(100);
    pub const SHOW: Self = Self(80);
    pub const QUEUE: Self = Self(60);
    pub const ROTATION: Self = Self(40);
    pub const RELAY: Self = Self(20);
    pub const FUN: Self = Self(1);
}
```

`Priority` ist bewusst kein geschlossenes Enum, weil die Architektur „Defaults, später pro Source konfigurierbar“ verlangt. Die DB erzwingt nur den Bereich `1..100`.

### 3.3 Zeitfelder und DST

**Speicherung:**

- Alle Instants (`starts_at`, `ends_at`, `recorded_at`, `published_at`, `scheduled_at`, `sent_at`, `occurred_at`, `created_at`, `updated_at`, `expires_at`, `played_at`) sind `TEXT` in UTC: `YYYY-MM-DDTHH:MM:SS.sssZ`.
- `default_schedule` ist kein Instant, sondern eine Liste von Wandzeit-Regeln. Jede Regel enthält `timezone: "Europe/Berlin"`.

**API:**

- Requests akzeptieren RFC3339 mit Offset. Fehlt der Offset, wird die Zeit als `Europe/Berlin` interpretiert.
- Responses liefern immer `Europe/Berlin` mit Offset, z. B. `2026-11-18T20:00:00+01:00`.
- `from`/`to`-Query-Parameter sind ebenfalls Berliner Zeit.

**DST-Policy:**

| Fall                                              | Verhalten                                                                                                                                 |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Lokale Zeit existiert nicht (Spring Forward)      | Auf den ersten gültigen Instant nach der Lücke verschieben. Beispiel: `02:30` → `03:30`. `schedule_log`-Event `dst_adjusted` mit Details. |
| Lokale Zeit ist mehrdeutig (Fall Back), Startzeit | Früheres Vorkommen wählen (Sommerzeit-Offset).                                                                                            |
| Lokale Zeit ist mehrdeutig (Fall Back), Endzeit   | Späteres Vorkommen wählen (Winterzeit-Offset).                                                                                            |
| Endzeit <= Startzeit nach DST-Anpassung           | Endzeit um die Lücke nach vorne verschieben; `dst_adjusted` loggen.                                                                       |

**Rust-Helfer:**

```rust
use chrono::{DateTime, LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Berlin;

pub fn berlin_to_utc(naive: NaiveDateTime, is_end: bool) -> DateTime<Utc> {
    match Berlin.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt.with_timezone(&Utc),
        LocalResult::Ambiguous(earlier, later) => {
            if is_end { later.with_timezone(&Utc) } else { earlier.with_timezone(&Utc) }
        }
        LocalResult::None => {
            // DST-Lücke: nach vorne verschieben. Die konkrete Lücke wird
            // über chrono-tz ermittelt; hier als Beispiel +1h.
            let shifted = naive + chrono::Duration::hours(1);
            Berlin.from_local_datetime(&shifted).earliest()
                .expect("shifted time must exist")
                .with_timezone(&Utc)
        }
    }
}

pub fn utc_to_berlin(utc: DateTime<Utc>) -> DateTime<chrono_tz::Tz> {
    utc.with_timezone(&Berlin)
}
```

**Beispiel `default_schedule`:**

```json
[
  {
    "weekday": "monday",
    "start_local": "20:00",
    "end_local": "22:00",
    "timezone": "Europe/Berlin",
    "valid_from": "2026-01-01",
    "valid_until": null
  }
]
```

Der Scheduler expandiert diese Regeln zu konkreten `broadcasts`. Die Expansion ist idempotent: existiert für `(show_id, starts_at)` bereits ein Broadcast, wird kein Duplikat angelegt.

### 3.4 DTOs

Die folgenden DTOs sind die JSON-Repräsentationen der API. Sie enthalten keine Passwort-Hashes, keine Token-Hashes und keine internen Stream-Credentials.

| DTO                       | Felder                                                                                                                                                                                                                     |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `CasterDto`               | `id`, `username`, `display_name`, `email?`, `role`, `avatar_url?`, `bio`, `is_active`, `created_at`, `updated_at`                                                                                                          |
| `RoleDto`                 | `id`, `name`, `description`, `permissions`, `is_system`                                                                                                                                                                    |
| `ShowDto`                 | `id`, `slug`, `name`, `description`, `artwork_url?`, `links`, `default_schedule`, `owner_caster_id?`, `created_by`, `created_at`, `updated_at`                                                                             |
| `BroadcastDto`            | `id`, `show_id?`, `caster_id`, `kind`, `title`, `description`, `starts_at`, `ends_at`, `status`, `tape_id?`, `queue_id?`, `version`, `created_by`, `created_at`, `updated_at`                                              |
| `SourceDto`               | `id`, `name`, `kind`, `priority`, `enabled`, `caster_id?`, `queue_id?`, `show_id?`, `config`, `created_at`, `updated_at`                                                                                                   |
| `QueueDto`                | `id`, `caster_id`, `name`, `description`, `kind`, `is_active`, `version`, `created_at`, `updated_at`                                                                                                                       |
| `QueueItemDto`            | `id`, `queue_id`, `position`, `track_id?`, `tape_id?`, `title_override?`, `state`, `played_at?`, `created_at`                                                                                                              |
| `TrackDto`                | `id`, `music_api_id`, `isrc?`, `title`, `artist`, `album`, `duration_ms`, `storage_key`, `artwork_url?`, `visibility`, `owner_caster_id?`, `metadata`, `created_at`, `updated_at`                                          |
| `TapeDto`                 | `id`, `title`, `description`, `caster_id`, `show_id?`, `broadcast_id?`, `music_api_id?`, `storage_key?`, `duration_ms`, `recorded_at`, `published_at?`, `status`, `created_at`, `updated_at`                               |
| `AnnouncementDto`         | `id`, `event`, `show_id?`, `broadcast_id?`, `caster_id?`, `template_id?`, `channel`, `payload`, `dedupe_key?`, `scheduled_at?`, `sent_at?`, `status`, `attempts`, `next_attempt_at?`, `error?`, `created_at`, `updated_at` |
| `AnnouncementTemplateDto` | `id`, `name`, `event`, `show_id?`, `channel`, `body`, `enabled`, `created_at`, `updated_at`                                                                                                                                |
| `ScheduleLogEntryDto`     | `id`, `occurred_at`, `event`, `broadcast_id?`, `source_id?`, `caster_id?`, `details`, `created_at`                                                                                                                         |
| `SessionDto`              | `id`, `caster_id`, `created_at`, `expires_at`, `last_seen_at?`, `revoked_at?`, `user_agent?`, `ip_address?`                                                                                                                |
| `NowPlayingDto`           | `source?`, `broadcast?`, `caster?`, `item?`, `started_at?`, `priority?`                                                                                                                                                    |
| `PlaybackItemDto`         | `queue_item_id?`, `kind` (`track`/`tape`), `track_id?`, `tape_id?`, `title`, `artist?`, `duration_ms`, `uri`, `cue_in_ms?`, `cue_out_ms?`                                                                                  |
| `LiveConnectionDto`       | `caster_id`, `caster_name`, `mount`, `connected_at`, `state`, `priority`                                                                                                                                                   |
| `ErrorDto`                | `error.code`, `error.message`, `error.details?`                                                                                                                                                                            |

**Payload-Konventionen:** Create- und Update-Payloads verwenden dieselben Felder wie die DTOs, aber ohne `id`, `created_at` und `updated_at`. Bei `PATCH` ist `version` Pflicht, wo optimistische Sperren gelten (`broadcasts`, `queues`). Passwörter werden nur bei Create und Password-Change übertragen.

**Beispiel `BroadcastDto`:**

```json
{
  "id": 10,
  "show_id": 1,
  "caster_id": 1,
  "kind": "live",
  "title": "Morning Radio",
  "description": "",
  "starts_at": "2026-11-18T08:00:00+01:00",
  "ends_at": "2026-11-18T10:00:00+01:00",
  "status": "scheduled",
  "tape_id": null,
  "queue_id": null,
  "version": 1,
  "created_by": 1,
  "created_at": "2026-11-01T12:00:00+01:00",
  "updated_at": "2026-11-01T12:00:00+01:00"
}
```

**Beispiel `PlaybackItemDto`:**

```json
{
  "queue_item_id": 123,
  "kind": "track",
  "track_id": 42,
  "tape_id": null,
  "title": "Song",
  "artist": "Artist",
  "duration_ms": 215000,
  "uri": "https://music-api.example/isrc/US-XXXX-XXXX/320",
  "cue_in_ms": null,
  "cue_out_ms": null
}
```

**Beispiel `ErrorDto`:**

```json
{
  "error": {
    "code": "validation_error",
    "message": "ends_at must be after starts_at",
    "details": { "field": "ends_at" }
  }
}
```

### 3.5 Scheduler-API

**Basis:** `http://scheduler:8080/api/v1` (intern, nicht öffentlich).
**Auth:** `Authorization: Bearer <service_token>` für das Portal; `X-Liquidsoap-Token` für Liquidsoap. Das Portal sendet zusätzlich `X-Caster-Id` und `X-Caster-Role`.

#### Allgemein

| Methode | Pfad      | Zweck                            | Erfolg | Fehler              |
| ------- | --------- | -------------------------------- | ------ | ------------------- |
| `GET`   | `/health` | Health-Check                     | `200`  | `500`               |
| `GET`   | `/now`    | Aktueller Zustand                | `200`  | `500`               |
| `GET`   | `/state`  | Vollständiger Zustand (Operator) | `200`  | `401`, `403`, `500` |

#### Sources

| Methode  | Pfad                    | Zweck        | Erfolg | Fehler                     |
| -------- | ----------------------- | ------------ | ------ | -------------------------- |
| `GET`    | `/sources`              | Liste        | `200`  | `401`, `500`               |
| `POST`   | `/sources`              | Anlegen      | `201`  | `401`, `403`, `409`, `422` |
| `GET`    | `/sources/{id}`         | Detail       | `200`  | `401`, `404`               |
| `PATCH`  | `/sources/{id}`         | Ändern       | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/sources/{id}`         | Löschen      | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/sources/{id}/enable`  | Aktivieren   | `200`  | `401`, `403`, `404`        |
| `POST`   | `/sources/{id}/disable` | Deaktivieren | `200`  | `401`, `403`, `404`        |

#### Shows

| Methode  | Pfad          | Zweck   | Erfolg | Fehler                     |
| -------- | ------------- | ------- | ------ | -------------------------- |
| `GET`    | `/shows`      | Liste   | `200`  | `401`                      |
| `POST`   | `/shows`      | Anlegen | `201`  | `401`, `409`, `422`        |
| `GET`    | `/shows/{id}` | Detail  | `200`  | `401`, `404`               |
| `PATCH`  | `/shows/{id}` | Ändern  | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/shows/{id}` | Löschen | `204`  | `401`, `403`, `404`, `409` |

#### Broadcasts und Schedule

| Methode  | Pfad                                                | Zweck                        | Erfolg | Fehler                            |
| -------- | --------------------------------------------------- | ---------------------------- | ------ | --------------------------------- |
| `GET`    | `/schedule?from=&to=`                               | Schedule im Zeitraum         | `200`  | `401`, `422`                      |
| `GET`    | `/broadcasts?from=&to=&show_id=&caster_id=&status=` | Liste                        | `200`  | `401`, `422`                      |
| `POST`   | `/broadcasts`                                       | Anlegen                      | `201`  | `401`, `409`, `422`               |
| `GET`    | `/broadcasts/{id}`                                  | Detail                       | `200`  | `401`, `404`                      |
| `PATCH`  | `/broadcasts/{id}`                                  | Ändern                       | `200`  | `401`, `403`, `404`, `409`, `422` |
| `DELETE` | `/broadcasts/{id}`                                  | Löschen                      | `204`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/broadcasts/{id}/cancel`                           | Absagen                      | `200`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/broadcasts/{id}/start`                            | Starten (Operator)           | `200`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/broadcasts/{id}/end`                              | Beenden (Operator)           | `200`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/schedule/expand?from=&to=`                        | Default-Schedule expandieren | `200`  | `401`, `403`, `422`               |

**Beispiel `POST /broadcasts`:**

```json
{
  "show_id": 1,
  "caster_id": 1,
  "kind": "live",
  "title": "Morning Radio",
  "description": "",
  "starts_at": "2026-11-18T08:00:00+01:00",
  "ends_at": "2026-11-18T10:00:00+01:00"
}
```

Antwort `201` mit `BroadcastDto`. Bei `kind = "recording"` ist `tape_id` erforderlich; bei `kind = "playlist"` ist `queue_id` erforderlich. Bei `kind = "live"` sind beide verboten.

#### Queues

| Methode  | Pfad                           | Zweck           | Erfolg | Fehler                            |
| -------- | ------------------------------ | --------------- | ------ | --------------------------------- |
| `GET`    | `/queues?caster_id=&kind=`     | Liste           | `200`  | `401`                             |
| `POST`   | `/queues`                      | Anlegen         | `201`  | `401`, `422`                      |
| `GET`    | `/queues/{id}`                 | Detail          | `200`  | `401`, `404`                      |
| `PATCH`  | `/queues/{id}`                 | Ändern          | `200`  | `401`, `403`, `404`, `409`, `422` |
| `DELETE` | `/queues/{id}`                 | Löschen         | `204`  | `401`, `403`, `404`, `409`        |
| `GET`    | `/queues/{id}/items`           | Items           | `200`  | `401`, `404`                      |
| `POST`   | `/queues/{id}/items`           | Item hinzufügen | `201`  | `401`, `404`, `409`, `422`        |
| `PATCH`  | `/queues/{id}/items/{item_id}` | Item ändern     | `200`  | `401`, `404`, `422`               |
| `DELETE` | `/queues/{id}/items/{item_id}` | Item löschen    | `204`  | `401`, `404`                      |
| `POST`   | `/queues/{id}/reorder`         | Neu ordnen      | `200`  | `401`, `404`, `409`, `422`        |
| `POST`   | `/queues/{id}/clear`           | Leeren          | `204`  | `401`, `404`                      |
| `POST`   | `/queues/{id}/advance`         | Nächstes Item   | `200`  | `401`, `404`, `409`               |

**Beispiel `POST /queues/{id}/items`:**

```json
{
  "track_id": 42,
  "tape_id": null,
  "title_override": null,
  "position": 1000
}
```

Genau eines von `track_id`/`tape_id` muss gesetzt sein. `position` ist optional; ohne Angabe wird ans Ende gelegt.

**Beispiel `POST /queues/{id}/reorder`:**

```json
{
  "item_ids": [103, 101, 102],
  "version": 4
}
```

`version` ist die optimistische Sperre der Queue. Bei Konflikt `409`.

#### Tracks

| Methode  | Pfad                                      | Zweck                | Erfolg      | Fehler                     |
| -------- | ----------------------------------------- | -------------------- | ----------- | -------------------------- |
| `GET`    | `/tracks?q=&visibility=&owner_caster_id=` | Liste                | `200`       | `401`                      |
| `POST`   | `/tracks`                                 | Registrieren         | `201`       | `401`, `409`, `422`        |
| `GET`    | `/tracks/{id}`                            | Detail               | `200`       | `401`, `404`               |
| `PATCH`  | `/tracks/{id}`                            | Ändern               | `200`       | `401`, `403`, `404`, `422` |
| `DELETE` | `/tracks/{id}`                            | Löschen              | `204`       | `401`, `403`, `404`, `409` |
| `POST`   | `/tracks/import`                          | Import aus Music API | `200`/`201` | `401`, `404`, `422`, `502` |

#### Tapes

| Methode  | Pfad                                 | Zweck           | Erfolg | Fehler                     |
| -------- | ------------------------------------ | --------------- | ------ | -------------------------- |
| `GET`    | `/tapes?show_id=&caster_id=&status=` | Liste           | `200`  | `401`                      |
| `POST`   | `/tapes`                             | Registrieren    | `201`  | `401`, `422`               |
| `GET`    | `/tapes/{id}`                        | Detail          | `200`  | `401`, `404`               |
| `PATCH`  | `/tapes/{id}`                        | Ändern          | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/tapes/{id}`                        | Löschen         | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/tapes/{id}/publish`                | Veröffentlichen | `200`  | `401`, `403`, `404`, `409` |
| `POST`   | `/tapes/{id}/unpublish`              | Zurückziehen    | `200`  | `401`, `403`, `404`, `409` |

#### Playback (Liquidsoap)

| Methode | Pfad                        | Zweck          | Erfolg      | Fehler              |
| ------- | --------------------------- | -------------- | ----------- | ------------------- |
| `GET`   | `/playback/next?source_id=` | Nächstes Item  | `200`/`204` | `401`, `404`, `422` |
| `GET`   | `/playback/current`         | Aktuelles Item | `200`/`204` | `401`               |
| `POST`  | `/playback/started`         | Start melden   | `204`       | `401`, `422`        |
| `POST`  | `/playback/ended`           | Ende melden    | `204`       | `401`, `422`        |
| `POST`  | `/playback/error`           | Fehler melden  | `204`       | `401`, `422`        |

**Beispiel `GET /playback/next?source_id=3`:**

```json
{
  "queue_item_id": 123,
  "kind": "track",
  "track_id": 42,
  "tape_id": null,
  "title": "Song",
  "artist": "Artist",
  "duration_ms": 215000,
  "uri": "https://music-api.example/isrc/US-XXXX-XXXX/320",
  "cue_in_ms": null,
  "cue_out_ms": null
}
```

`204 No Content` bedeutet: aktuell kein abspielbares Item. Liquidsoap soll den Fallback-/Fun-Layer spielen.

`queue_item_id` ist gesetzt, wenn das Item aus einer Queue stammt; bei Rotation/Relay/Fun kann es `null` sein. `track_id`/`tape_id` identifizieren das Medienobjekt. State-Transitions (`started`, `ended`, `error`) verwenden `queue_item_id`, damit dasselbe Medium mehrfach in einer Queue vorkommen kann.

**Beispiel `POST /playback/started`:**

```json
{
  "queue_item_id": 123,
  "kind": "track",
  "track_id": 42,
  "source_id": 3,
  "started_at": "2026-11-18T20:00:00+01:00"
}
```

#### Live und Control

| Methode | Pfad                | Zweck              | Erfolg | Fehler                     |
| ------- | ------------------- | ------------------ | ------ | -------------------------- |
| `GET`   | `/live`             | Verbundene Caster  | `200`  | `401`                      |
| `POST`  | `/live/select`      | Caster auswählen   | `200`  | `401`, `403`, `409`, `422` |
| `POST`  | `/live/next`        | Nächsten Caster    | `200`  | `401`, `403`, `409`        |
| `POST`  | `/control/switch`   | Source wechseln    | `200`  | `401`, `403`, `409`, `422` |
| `POST`  | `/control/fallback` | Fallback erzwingen | `200`  | `401`, `403`, `409`        |
| `GET`   | `/control/state`    | Zustand            | `200`  | `401`, `403`               |

**Beispiel `POST /live/select`:**

```json
{
  "caster_id": 1,
  "reason": "operator_override"
}
```

**Beispiel `POST /control/switch`:**

```json
{
  "source_id": 2,
  "reason": "manual"
}
```

#### Announcements

| Methode  | Pfad                                      | Zweck   | Erfolg | Fehler                     |
| -------- | ----------------------------------------- | ------- | ------ | -------------------------- |
| `GET`    | `/announcements?status=&from=&to=&event=` | Liste   | `200`  | `401`, `422`               |
| `POST`   | `/announcements`                          | Anlegen | `201`  | `401`, `422`               |
| `GET`    | `/announcements/{id}`                     | Detail  | `200`  | `401`, `404`               |
| `PATCH`  | `/announcements/{id}`                     | Ändern  | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/announcements/{id}`                     | Löschen | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/announcements/{id}/send`                | Senden  | `200`  | `401`, `403`, `404`, `409` |
| `GET`    | `/announcement-templates`                 | Liste   | `200`  | `401`                      |
| `POST`   | `/announcement-templates`                 | Anlegen | `201`  | `401`, `403`, `409`, `422` |
| `GET`    | `/announcement-templates/{id}`            | Detail  | `200`  | `401`, `404`               |
| `PATCH`  | `/announcement-templates/{id}`            | Ändern  | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/announcement-templates/{id}`            | Löschen | `204`  | `401`, `403`, `404`, `409` |

**Beispiel `POST /announcements`:**

```json
{
  "event": "live_on",
  "show_id": 1,
  "broadcast_id": 10,
  "caster_id": 1,
  "template_id": 1,
  "channel": "telegram",
  "payload": {
    "show_name": "Morning Radio",
    "caster_name": "Alice",
    "start_time": "08:00"
  },
  "dedupe_key": "live_on:10",
  "scheduled_at": "2026-11-18T07:55:00+01:00"
}
```

#### Schedule-Log

| Methode | Pfad                                                      | Zweck  | Erfolg | Fehler              |
| ------- | --------------------------------------------------------- | ------ | ------ | ------------------- |
| `GET`   | `/schedule-log?from=&to=&event=&broadcast_id=&caster_id=` | Liste  | `200`  | `401`, `403`, `422` |
| `GET`   | `/schedule-log/{id}`                                      | Detail | `200`  | `401`, `403`, `404` |

#### Casters, Rollen, interne Auth

| Methode  | Pfad                               | Zweck                       | Erfolg | Fehler                     |
| -------- | ---------------------------------- | --------------------------- | ------ | -------------------------- |
| `GET`    | `/casters`                         | Liste                       | `200`  | `401`, `403`               |
| `POST`   | `/casters`                         | Anlegen                     | `201`  | `401`, `403`, `409`, `422` |
| `GET`    | `/casters/{id}`                    | Detail                      | `200`  | `401`, `403`, `404`        |
| `PATCH`  | `/casters/{id}`                    | Ändern                      | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/casters/{id}`                    | Löschen                     | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/casters/{id}/stream-credentials` | Stream-Credentials rotieren | `200`  | `401`, `403`, `404`        |
| `GET`    | `/roles`                           | Liste                       | `200`  | `401`, `403`               |
| `PATCH`  | `/roles/{id}`                      | Permissions ändern          | `200`  | `401`, `403`, `404`, `422` |
| `POST`   | `/internal/auth/verify`            | Credentials prüfen          | `200`  | `401`, `422`, `429`        |
| `GET`    | `/internal/casters/{id}`           | Caster für Portal           | `200`  | `401`, `404`               |

**Beispiel `POST /internal/auth/verify`:**

```json
{
  "username": "alice",
  "password": "correct horse battery staple"
}
```

Antwort `200`:

```json
{
  "caster": {
    "id": 1,
    "username": "alice",
    "display_name": "Alice",
    "email": "alice@example.org",
    "role": "caster",
    "avatar_url": null,
    "bio": "",
    "is_active": true,
    "created_at": "2026-11-01T12:00:00+01:00",
    "updated_at": "2026-11-01T12:00:00+01:00"
  }
}
```

### 3.6 Portal-API

**Basis:** `https://radio.example/api/v1` (öffentlich).
**Auth:** Session-Cookie (`session`, HttpOnly, Secure, SameSite=Lax) oder `Authorization: Bearer <session_token>` für API-Clients.

#### Auth

| Methode  | Pfad                  | Zweck              | Erfolg | Fehler              |
| -------- | --------------------- | ------------------ | ------ | ------------------- |
| `POST`   | `/auth/login`         | Login              | `200`  | `401`, `422`, `429` |
| `POST`   | `/auth/logout`        | Logout             | `204`  | `401`               |
| `GET`    | `/auth/me`            | Eigenes Profil     | `200`  | `401`               |
| `PATCH`  | `/auth/me`            | Profil ändern      | `200`  | `401`, `422`        |
| `POST`   | `/auth/password`      | Passwort ändern    | `204`  | `401`, `422`        |
| `GET`    | `/auth/sessions`      | Eigene Sessions    | `200`  | `401`               |
| `DELETE` | `/auth/sessions/{id}` | Session widerrufen | `204`  | `401`, `403`, `404` |

**Beispiel `POST /auth/login`:**

```json
{
  "username": "alice",
  "password": "correct horse battery staple"
}
```

Antwort `200`:

```json
{
  "session_token": "opaque-token-only-once",
  "expires_at": "2026-11-25T20:00:00+01:00",
  "caster": {
    "id": 1,
    "username": "alice",
    "display_name": "Alice",
    "email": "alice@example.org",
    "role": "caster",
    "avatar_url": null,
    "bio": "",
    "is_active": true,
    "created_at": "2026-11-01T12:00:00+01:00",
    "updated_at": "2026-11-01T12:00:00+01:00"
  }
}
```

Das Portal setzt zusätzlich das HttpOnly-Cookie. Der Token wird nur einmal im Response-Body ausgegeben.

#### Casters

| Methode  | Pfad                               | Zweck              | Erfolg | Fehler                     |
| -------- | ---------------------------------- | ------------------ | ------ | -------------------------- |
| `GET`    | `/casters`                         | Liste              | `200`  | `401`, `403`               |
| `POST`   | `/casters`                         | Anlegen            | `201`  | `401`, `403`, `409`, `422` |
| `GET`    | `/casters/{id}`                    | Detail             | `200`  | `401`, `403`, `404`        |
| `PATCH`  | `/casters/{id}`                    | Ändern             | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/casters/{id}`                    | Löschen            | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/casters/{id}/stream-credentials` | Stream-Credentials | `200`  | `401`, `403`, `404`        |

#### Shows

| Methode  | Pfad          | Zweck   | Erfolg | Fehler                     |
| -------- | ------------- | ------- | ------ | -------------------------- |
| `GET`    | `/shows`      | Liste   | `200`  | `401`                      |
| `POST`   | `/shows`      | Anlegen | `201`  | `401`, `409`, `422`        |
| `GET`    | `/shows/{id}` | Detail  | `200`  | `401`, `404`               |
| `PATCH`  | `/shows/{id}` | Ändern  | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/shows/{id}` | Löschen | `204`  | `401`, `403`, `404`, `409` |

#### Broadcasts

| Methode  | Pfad                                                | Zweck                        | Erfolg | Fehler                            |
| -------- | --------------------------------------------------- | ---------------------------- | ------ | --------------------------------- |
| `GET`    | `/schedule?from=&to=`                               | Schedule im Zeitraum         | `200`  | `401`, `422`                      |
| `GET`    | `/broadcasts?from=&to=&show_id=&caster_id=&status=` | Liste                        | `200`  | `401`, `422`                      |
| `POST`   | `/broadcasts`                                       | Anlegen                      | `201`  | `401`, `409`, `422`               |
| `GET`    | `/broadcasts/{id}`                                  | Detail                       | `200`  | `401`, `404`                      |
| `PATCH`  | `/broadcasts/{id}`                                  | Ändern                       | `200`  | `401`, `403`, `404`, `409`, `422` |
| `DELETE` | `/broadcasts/{id}`                                  | Löschen                      | `204`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/broadcasts/{id}/cancel`                           | Absagen                      | `200`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/broadcasts/{id}/start`                            | Starten                      | `200`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/broadcasts/{id}/end`                              | Beenden                      | `200`  | `401`, `403`, `404`, `409`        |
| `POST`   | `/schedule/expand?from=&to=`                        | Default-Schedule expandieren | `200`  | `401`, `403`, `422`               |

#### Queues

| Methode  | Pfad                           | Zweck           | Erfolg | Fehler                            |
| -------- | ------------------------------ | --------------- | ------ | --------------------------------- |
| `GET`    | `/queues?caster_id=&kind=`     | Liste           | `200`  | `401`                             |
| `POST`   | `/queues`                      | Anlegen         | `201`  | `401`, `422`                      |
| `GET`    | `/queues/{id}`                 | Detail          | `200`  | `401`, `403`, `404`               |
| `PATCH`  | `/queues/{id}`                 | Ändern          | `200`  | `401`, `403`, `404`, `409`, `422` |
| `DELETE` | `/queues/{id}`                 | Löschen         | `204`  | `401`, `403`, `404`, `409`        |
| `GET`    | `/queues/{id}/items`           | Items           | `200`  | `401`, `403`, `404`               |
| `POST`   | `/queues/{id}/items`           | Item hinzufügen | `201`  | `401`, `403`, `404`, `409`, `422` |
| `PATCH`  | `/queues/{id}/items/{item_id}` | Item ändern     | `200`  | `401`, `403`, `404`, `422`        |
| `DELETE` | `/queues/{id}/items/{item_id}` | Item löschen    | `204`  | `401`, `403`, `404`               |
| `POST`   | `/queues/{id}/reorder`         | Neu ordnen      | `200`  | `401`, `403`, `404`, `409`, `422` |
| `POST`   | `/queues/{id}/clear`           | Leeren          | `204`  | `401`, `403`, `404`               |
| `POST`   | `/queues/{id}/advance`         | Nächstes Item   | `200`  | `401`, `403`, `404`, `409`        |

#### Tracks

| Methode  | Pfad                                      | Zweck        | Erfolg      | Fehler                     |
| -------- | ----------------------------------------- | ------------ | ----------- | -------------------------- |
| `GET`    | `/tracks?q=&visibility=&owner_caster_id=` | Liste        | `200`       | `401`                      |
| `POST`   | `/tracks`                                 | Registrieren | `201`       | `401`, `409`, `422`        |
| `POST`   | `/tracks/upload`                          | Upload       | `201`       | `401`, `413`, `422`, `502` |
| `GET`    | `/tracks/{id}`                            | Detail       | `200`       | `401`, `404`               |
| `PATCH`  | `/tracks/{id}`                            | Ändern       | `200`       | `401`, `403`, `404`, `422` |
| `DELETE` | `/tracks/{id}`                            | Löschen      | `204`       | `401`, `403`, `404`, `409` |
| `POST`   | `/tracks/import`                          | Import       | `200`/`201` | `401`, `404`, `422`, `502` |

#### Tapes

| Methode  | Pfad                                 | Zweck           | Erfolg | Fehler                     |
| -------- | ------------------------------------ | --------------- | ------ | -------------------------- |
| `GET`    | `/tapes?show_id=&caster_id=&status=` | Liste           | `200`  | `401`                      |
| `POST`   | `/tapes`                             | Registrieren    | `201`  | `401`, `422`               |
| `POST`   | `/tapes/upload`                      | Upload          | `201`  | `401`, `413`, `422`, `502` |
| `GET`    | `/tapes/{id}`                        | Detail          | `200`  | `401`, `404`               |
| `PATCH`  | `/tapes/{id}`                        | Ändern          | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/tapes/{id}`                        | Löschen         | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/tapes/{id}/publish`                | Veröffentlichen | `200`  | `401`, `403`, `404`, `409` |
| `POST`   | `/tapes/{id}/unpublish`              | Zurückziehen    | `200`  | `401`, `403`, `404`, `409` |

#### Announcements

| Methode  | Pfad                                      | Zweck   | Erfolg | Fehler                     |
| -------- | ----------------------------------------- | ------- | ------ | -------------------------- |
| `GET`    | `/announcements?status=&from=&to=&event=` | Liste   | `200`  | `401`, `422`               |
| `POST`   | `/announcements`                          | Anlegen | `201`  | `401`, `422`               |
| `GET`    | `/announcements/{id}`                     | Detail  | `200`  | `401`, `404`               |
| `PATCH`  | `/announcements/{id}`                     | Ändern  | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/announcements/{id}`                     | Löschen | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/announcements/{id}/send`                | Senden  | `200`  | `401`, `403`, `404`, `409` |
| `GET`    | `/announcement-templates`                 | Liste   | `200`  | `401`                      |
| `POST`   | `/announcement-templates`                 | Anlegen | `201`  | `401`, `403`, `409`, `422` |
| `GET`    | `/announcement-templates/{id}`            | Detail  | `200`  | `401`, `404`               |
| `PATCH`  | `/announcement-templates/{id}`            | Ändern  | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/announcement-templates/{id}`            | Löschen | `204`  | `401`, `403`, `404`, `409` |

#### Live, Control, Schedule-Log

| Methode | Pfad                             | Zweck              | Erfolg | Fehler                     |
| ------- | -------------------------------- | ------------------ | ------ | -------------------------- |
| `GET`   | `/live`                          | Verbundene Caster  | `200`  | `401`                      |
| `POST`  | `/live/select`                   | Caster auswählen   | `200`  | `401`, `403`, `409`, `422` |
| `POST`  | `/live/next`                     | Nächsten Caster    | `200`  | `401`, `403`, `409`        |
| `GET`   | `/now`                           | Aktueller Zustand  | `200`  | `401`                      |
| `POST`  | `/control/switch`                | Source wechseln    | `200`  | `401`, `403`, `409`, `422` |
| `POST`  | `/control/fallback`              | Fallback erzwingen | `200`  | `401`, `403`, `409`        |
| `GET`   | `/control/state`                 | Zustand            | `200`  | `401`, `403`               |
| `GET`   | `/schedule-log?from=&to=&event=` | Log                | `200`  | `401`, `403`, `422`        |
| `GET`   | `/schedule-log/{id}`             | Log-Detail         | `200`  | `401`, `403`, `404`        |

#### Sources und Rollen (Operator/Admin)

| Methode  | Pfad                    | Zweck              | Erfolg | Fehler                     |
| -------- | ----------------------- | ------------------ | ------ | -------------------------- |
| `GET`    | `/sources`              | Liste              | `200`  | `401`, `403`               |
| `POST`   | `/sources`              | Anlegen            | `201`  | `401`, `403`, `409`, `422` |
| `PATCH`  | `/sources/{id}`         | Ändern             | `200`  | `401`, `403`, `404`, `422` |
| `DELETE` | `/sources/{id}`         | Löschen            | `204`  | `401`, `403`, `404`, `409` |
| `POST`   | `/sources/{id}/enable`  | Aktivieren         | `200`  | `401`, `403`, `404`        |
| `POST`   | `/sources/{id}/disable` | Deaktivieren       | `200`  | `401`, `403`, `404`        |
| `GET`    | `/roles`                | Liste              | `200`  | `401`, `403`               |
| `PATCH`  | `/roles/{id}`           | Permissions ändern | `200`  | `401`, `403`, `404`, `422` |

### 3.7 Auth, Fehler, Statuscodes

**Portal-Auth:**

- Passwort-Hashing: Argon2id mit per-User-Salt.
- Session-Token: 32 Byte Zufall, base64url; in der DB nur SHA-256.
- Cookie: `session`, `HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`.
- Session-Lifetime: 7 Tage, gleitend bei Nutzung; `expires_at` wird bei Bedarf verlängert.
- Login-Rate-Limit: z. B. 5 Fehlversuche pro 15 Minuten pro IP/Username.
- CSRF: SameSite=Lax plus Double-Submit-Token für state-changing Requests.
- Logout: Session wird revoked, Cookie gelöscht.

**Scheduler-Auth:**

- Portal → Scheduler: `Authorization: Bearer <scheduler_service_token>`.
- Zusätzlich `X-Caster-Id` und `X-Caster-Role` für User-Kontext.
- Liquidsoap → Scheduler: `X-Liquidsoap-Token`.
- Die Scheduler-API lauscht nur auf localhost/privatem Netz.
- Interne Auth-Verifikation (`/internal/auth/verify`) ist rate-limitiert.

**Statuscodes:**

| Code  | Bedeutung                           |
| ----- | ----------------------------------- |
| `200` | OK                                  |
| `201` | Created                             |
| `204` | No Content                          |
| `400` | Malformed Request (JSON/Query)      |
| `401` | Unauthorized                        |
| `403` | Forbidden                           |
| `404` | Not Found                           |
| `409` | Conflict (Duplikat, Version, State) |
| `413` | Payload Too Large                   |
| `422` | Validation Error                    |
| `429` | Too Many Requests                   |
| `500` | Internal Server Error               |
| `502` | Bad Gateway (Music API)             |
| `503` | Service Unavailable                 |

**Error-Envelope:**

```json
{
  "error": {
    "code": "validation_error",
    "message": "human readable message",
    "details": {
      "field": "starts_at",
      "reason": "must be before ends_at"
    }
  }
}
```

**Idempotenz und Pagination:**

- `POST`-Create-Endpunkte akzeptieren optional `Idempotency-Key`.
- Listen-Endpunkte akzeptieren `limit` (Default 50, Max 200) und `offset`.
- Zeitfilter `from`/`to` sind inklusiv/exklusiv: `from <= t < to`.

### 3.8 Migrationsstrategie (sqlx)

**Verzeichnisse:**

- `scheduler/migrations/`
- `portal/migrations/`

**Namensschema:**

- `YYYYMMDDHHMMSS_kurze_beschreibung.sql` (sqlx-Default).
- Jede Migration ist append-only. Bereits angewendete Migrationen werden nie editiert.

**Ablauf:**

1. Vor jeder Migration ein Backup erstellen:
   ```sh
   sqlite3 scheduler.db ".backup 'scheduler.db.bak'"
   sqlite3 portal.db ".backup 'portal.db.bak'"
   ```
2. Migrationen im CI gegen eine leere DB und gegen eine Kopie der Produktions-DB testen.
3. Deployment führt `sqlx migrate run` beim Start des jeweiligen Service aus.
4. `sqlx migrate info` zeigt den Status.
5. Bei Fehlern: Service stoppen, Backup zurückrollen, Migration korrigieren.

**SQLite-spezifische Regeln:**

- `journal_mode=WAL` und `foreign_keys=ON` werden über `SqliteConnectOptions` gesetzt, nicht in Migrationen.
- DDL ist in SQLite transaktional. Kurze Migrationen bevorzugen.
- Für Tabellen-Rewrites (SQLite kann Spalten nicht immer direkt ändern) die offizielle 12-Schritt-Prozedur verwenden: neue Tabelle, Daten kopieren, alte Tabelle droppen, neue umbenennen, Indizes/Trigger neu anlegen.
- `STRICT`-Tabellen und `CHECK`-Constraints früh testen; SQLite validiert bestehende Daten bei `STRICT`-Umstellung nicht automatisch.
- `VACUUM` nicht in Migrationen; separat als Wartungsjob.
- `sqlx::query!` mit Offline-Metadata (`.sqlx/`) oder `sqlx::query` ohne Compile-Time-Check. Für den Start ist `query` pragmatisch; später auf `query!` mit `cargo sqlx prepare` wechseln.

**Seed-Migrationen:**

- Rollen und generische Sources werden per `INSERT ... ON CONFLICT DO NOTHING` angelegt.
- Keine Seed-Daten für Casters, Shows oder Tracks in Produktionsmigrationen. Diese gehören in ein separates Dev-Seed-Skript.

## 4. Offene Fragen

1. **Shared vs. private Library:** Welche Sichtbarkeitsregeln gelten für `tracks.visibility = 'private'`? Dürfen Operator/Admin private Tracks sehen? Dürfen private Tracks in einer Show eines anderen Casters gespielt werden?
2. **Music API Integration:** Wird die lokale `tracks`-Tabelle per Webhook, Polling oder bei Bedarf synchronisiert? Was passiert bei Metadata-Änderungen oder Löschungen in der Music API?
3. **Upload-Flow:** Lädt das Portal Dateien direkt zur Music API hoch und registriert danach den Track/Tape, oder proxied das Portal die Bytes? Wie werden große Dateien und Resumable Uploads behandelt?
4. **Rollenmatrix:** Welche Permissions sind für `operator` und `caster` genau gewollt? Reicht die permissive Startmatrix, oder braucht es z. B. `shows.publish`, `tapes.publish`, `announcements.templates.manage`?
5. **Titelzeit-Regeln:** Die Architektur nennt „manuell > Zeitregel > DJ-Default > Fallback“. Wo werden Zeitregeln gespeichert? Eigene Tabelle `title_rules` oder JSON in `shows`/`sources`?
6. **Überlappende Broadcasts:** Sind Überlappungen erlaubt? Wenn ja, wie entscheidet der Scheduler zwischen zwei gleichzeitigen Live-Broadcasts? Wenn nein, soll `POST /broadcasts` mit `409` ablehnen?
7. **Queue-Semantik:** Startet eine `slot`-Queue bei Broadcast-Beginn immer bei Position 0? Was passiert mit der `background`-Queue während eines Live-/Show-Broadcasts? Wird sie pausiert, gemutet oder weitergespielt?
8. **Live-Handoff:** First come, first served ist klar. Aber wie wird die Reihenfolge bei mehreren gleichzeitigen Verbindungen ermittelt? Zählt die TCP-Verbindungszeit, die erste Audio-Frame-Zeit oder die Anmeldung im Portal? Wie lange ist ein Disconnect „tolerant“?
9. **Announcement-Timings:** Welche globalen und per-Show-Offsets gelten für `show_reminder`, `live_on`, `live_off`? Sollen Announcements bei fehlendem Template automatisch generiert oder nur geloggt werden?
10. **Tape-Veröffentlichung:** Wer darf Tapes veröffentlichen? Automatisch nach Broadcast-Ende oder manuell? Gibt es eine öffentliche Feed-API, die nicht in diesem Dokument beschrieben ist?
11. **Relay/Fallback:** Welche Relay-URLs sind erlaubt? Wie oft wird die Erreichbarkeit geprüft? Was ist der endgültige Fallback, wenn auch der Fun-Layer nicht verfügbar ist?
12. **HLS/Recording:** Segmentlänge, Codec, Bitrate und Aufbewahrungsdauer für Tapes. Wo werden HLS-Manifeste gespeichert?
13. **Session-Policy:** 7 Tage Lifetime und gleitende Verlängerung okay? Braucht es 2FA für Admin/Operator? Sollen Sessions an IP/User-Agent gebunden werden?
14. **ID-Strategie:** Bleibt es bei Integer-IDs, oder brauchen öffentliche APIs stabile UUIDs/ULIDs? Wenn ja, wann?
15. **SQLite-Skalierung:** Reicht eine SQLite-DB für Scheduler und eine für Portal? Ab welcher Schreiblast wird auf Postgres migriert?
16. **DST-Policy:** Ist die gewählte Regel (Start früher, Ende später, Lücke nach vorne) fachlich bestätigt? Sollen DST-Anpassungen zusätzlich per Announcement kommuniziert werden?
17. **Retention:** Wie lange werden `schedule_log`, `announcements` und `sessions` aufbewahrt? Wohin wird archiviert?
18. **Music-API-Ausfall:** Soll der Scheduler bei nicht erreichbarer Music API auf lokal gecachte Dateien ausweichen? Wie lange dürfen signed URLs gültig sein?
19. **API-Vertrag:** Wird OpenAPI aus Rust generiert (z. B. `utoipa`) oder ein gemeinsames `shared`-Crate für DTOs verwendet? Beides ist möglich; die Entscheidung beeinflusst Contract-Tests.
20. **Stream-Credentials:** Wie authentifiziert Liquidsoap/Icecast die Caster? Werden Stream-Passwörter im Klartext benötigt oder reicht ein Auth-Endpunkt? Wie oft rotiert?
21. **Über-Mitternacht-Shows:** Wie werden `default_schedule`-Regeln mit `end_local < start_local` expandiert? Gehört das Ende zum Folgetag, und wie wird das mit `valid_from`/`valid_until` und DST kombiniert?

## 5. Risiken

| Risiko                               | Auswirkung                                              | Gegenmaßnahme                                                                                                     |
| ------------------------------------ | ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| SQLite-Schreibcontention             | Latenz, Timeouts bei parallelen Writes                  | WAL, kurze Transaktionen, getrennte DBs, `busy_timeout`, Monitoring. Bei Bedarf Postgres-Migration vorbereiten.   |
| DST-Bugs                             | Falsche Broadcast-Zeiten, doppelte/fehlende Slots       | `chrono-tz`, zentrale Helfer, Tests um Spring-/Fall-Transition, `dst_adjusted`-Logging.                           |
| Zirkuläre FKs Tape/Broadcast         | Insert-Fehler, verwaiste Referenzen                     | Deferred FKs, definierte Insert-Reihenfolge, Integrationstests.                                                   |
| Music-API-Ausfall                    | Kein Playback, keine Uploads, keine signed URLs         | Health-Checks, Retry mit Backoff, Metadaten-Cache, Fallback-Layer, klare 502-Fehler.                              |
| Live-Handoff-Races                   | Falscher Caster wird ausgegeben, Flackern               | Single-Writer im Scheduler, idempotente State-Transitions, `schedule_log`, Operator-Override.                     |
| Queue-Reordering-Konflikte           | Verlorene Updates, doppelte Positionen                  | Optimistische Sperre (`version`), transaktionales Reorder, Positionslücken, `409` bei Konflikt.                   |
| Announcement-Duplikate               | Spam, Vertrauensverlust                                 | `dedupe_key`, Idempotency-Key, Retry mit `attempts`/`next_attempt_at`, Dead-Letter-Log.                           |
| Session-Leak                         | Account-Übernahme                                       | HttpOnly/Secure/SameSite, Token-Hash, kurze Lifetime, Revocation, Rate-Limits, CSP.                               |
| Permissives RBAC                     | Unautorisierte Aktionen                                 | Permissions in `roles`, serverseitige Checks in Portal und Scheduler, Audit-Log, Default-Deny für Admin-Aktionen. |
| Migrationen mit Datenverlust         | Ausfall, Datenverlust                                   | Backups vor Migration, Forward-only, Test gegen DB-Kopie, 12-Schritt-Prozedur bei Rewrites.                       |
| Zeitzonen-Datenbank veraltet         | Falsche Offsets nach tzdb-Update                        | `chrono-tz` regelmäßig aktualisieren, DST-Tests in CI.                                                            |
| API-Contract-Drift                   | Portal und Scheduler sprechen unterschiedliche Sprachen | Gemeinsames `shared`-Crate oder OpenAPI, Contract-Tests, Versionierung.                                           |
| SQLite-Backup/Restore-Fehler         | Korruption, Datenverlust                                | `VACUUM INTO`/`.backup`, Restore-Tests, Disk-Monitoring, WAL-Checkpoint.                                          |
| Stream-Credential-Leak               | Unautorisierter Live-Zugang                             | Hashes, Secrets-Management, Rotation, keine Logs, getrennte Stream- und Portal-Passwörter.                        |
| Überlappende Broadcasts unklar       | Unerwartete Ausgabe, Konflikte                          | Policy festlegen, Scheduler-Entscheidung dokumentieren, Tests.                                                    |
| Queue-Item-Löschung während Playback | Inkonsistenter Zustand                                  | Soft-Delete/State statt hartem Delete, Scheduler behandelt fehlende Items als `skipped`.                          |
| Tape/Broadcast-Löschung              | Verwaiste Tapes oder fehlende Provenienz                | `ON DELETE SET NULL`, Status `archived`, Retention-Policy.                                                        |
| Telegram-Rate-Limits                 | Announcements kommen zu spät oder gar nicht             | Queue, Backoff, Rate-Limit, Retry, Monitoring.                                                                    |
| HLS-/Tape-Storage-Wachstum           | Volle Disk                                              | Retention, Music-API-Storage, Monitoring, Alarmierung.                                                            |
| ID-Wiederverwendung (rowid)          | Audit-Referenzen zeigen auf neue Objekte                | `AUTOINCREMENT` für `casters`, `broadcasts`, `sources`; `schedule_log` ohne FKs.                                  |

## 6. Umsetzungsschritte

1. **Workspace anlegen:** Crates `scheduler`, `portal`, `shared` (DTOs/Enums/Zeithelfer). `shared` enthält keine DB-Verbindung; es nutzt `sqlx` nur für `Type`-Derives.
2. **Dependencies:** `axum`, `sqlx` (SQLite, chrono, json), `serde`, `chrono`, `chrono-tz`, `argon2`, `tracing`, `uuid`/`rand`, `reqwest` (Music API/Telegram).
3. **Scheduler-Migrationen:** Initiales Schema aus 3.1.1, Indizes, Trigger, Seed-Rollen und Seed-Sources.
4. **Portal-Migration:** `sessions` aus 3.1.2.
5. **Shared Types:** Enums, `Priority`, DTOs, `ErrorDto`, Zeithelfer (`berlin_to_utc`, `utc_to_berlin`, DST-Policy).
6. **Scheduler-Repositories:** Transaktionen für Broadcasts, Queues, Queue-Items, Tapes, Announcements; optimistische Sperren für `queues.version` und `broadcasts.version`.
7. **Scheduler-API:** Health, Now, State, Sources, Shows, Broadcasts/Schedule, Queues, Tracks, Tapes, Playback, Live/Control, Announcements, Schedule-Log, Casters/Rollen, interne Auth.
8. **Portal-API:** Auth/Sessions, Proxy zu Scheduler, Permission-Checks, Upload zu Music API, Announcement-Proxy.
9. **Auth:** Argon2id, Session-Token, Cookie, CSRF, Rate-Limits, Service-Token, Caster-Kontext.
10. **Zeitlogik:** `chrono-tz`-Helfer, DST-Tests, Schedule-Expansion aus `default_schedule`, `dst_adjusted`-Logging.
11. **Queue-Ordering:** Positionslücken, Reorder-Endpunkt, `version`-Konfliktbehandlung, Tests für parallele Reorders.
12. **Live-State-Machine:** Harbor-Verbindungen, First-come-first-served, Operator-Override, Disconnect-Handoff, `schedule_log`.
13. **Announcement-Worker:** Template-Rendering, Dedupe, Retry, Telegram-Versand, Fehlerlogging.
14. **Music-API-Client:** Upload, Import, Metadata-Sync, signed URLs, Retry/Health.
15. **Tests:** Unit-Tests für Enums/Zeit, Integrationstests mit temporärer SQLite-DB, API-Contract-Tests, DST-Transition-Tests, Concurrency-Tests für Queue/Reorder.
16. **OpenAPI/Contract:** Entscheidung `utoipa` vs. `shared`-Crate; Contract-Tests zwischen Portal und Scheduler.
17. **Betrieb:** Backups, Migration-Runbook, Monitoring, Alerts, Log-Retention, Restore-Test.
18. **Deployment:** systemd/Caddy, Env-Secrets, Zero-Downtime-Strategie, Smoke-Tests nach Deploy.

## 7. Akzeptanzkriterien

- [ ] `sqlx migrate run` erzeugt auf einer leeren DB alle Tabellen, Indizes und Trigger aus 3.1.1 und 3.1.2.
- [ ] `PRAGMA foreign_keys=ON` ist auf jeder Connection aktiv; FK-Verletzungen werden abgelehnt.
- [ ] Alle Enums aus 3.2 entsprechen exakt den `CHECK`-Constraints; ungültige Werte werden mit `422`/DB-Fehler abgelehnt.
- [ ] `schedule_log` ist append-only; `UPDATE` und `DELETE` werden durch Trigger abgelehnt.
- [ ] `broadcasts` erzwingt `ends_at > starts_at` und die Kind-/Content-Kombinationen.
- [ ] `queue_items` erzwingt genau eines von `track_id`/`tape_id`.
- [ ] `announcement_templates` erzwingt genau ein globales und ein per-Show-Template pro Event/Kanal.
- [ ] `announcements.dedupe_key` verhindert Duplikate; Retry-Felder werden korrekt fortgeschrieben.
- [ ] API liefert alle Zeitfelder in `Europe/Berlin` mit Offset; DB speichert UTC.
- [ ] DST-Tests für Spring Forward und Fall Back laufen grün; `dst_adjusted` wird geloggt.
- [ ] `POST /broadcasts` validiert Kind, Content und Zeitfenster; Fehler kommen als `422` mit Error-Envelope.
- [ ] `POST /queues/{id}/reorder` ist atomar; veraltete `version` liefert `409`.
- [ ] `POST /schedule/expand` ist idempotent; der Unique-Index `uq_broadcasts_show_start` verhindert Duplikate.
- [ ] `GET /playback/next` liefert das korrekte nächste Item oder `204`; `started`/`ended`/`error` aktualisieren den Zustand idempotent.
- [ ] `PlaybackItemDto` enthält `queue_item_id`; Zustandsänderungen werden dem richtigen Queue-Item zugeordnet.
- [ ] Live-Auswahl ist first come, first served; Operator-Override funktioniert; Disconnect führt zu definiertem Handoff.
- [ ] Announcements werden dedupliziert, bei Fehlern retried und im `schedule_log` protokolliert.
- [ ] Sessions laufen ab, können widerrufen werden und werden nur gehasht gespeichert.
- [ ] RBAC: Admin/Operator/Caster-Permissions werden in Portal und Scheduler durchgesetzt; permissive Defaults sind dokumentiert.
- [ ] Portal und Scheduler teilen dieselben DTOs/Enums (shared Crate oder OpenAPI); Contract-Tests laufen grün.
- [ ] Migrationen sind append-only; Backup/Restore ist getestet; ein Rollback-Runbook existiert.
- [ ] Keine Referenzen auf frühere Systeme, keine Altlasten-Imports, keine Kompatibilitäts-Shims.
- [ ] API-Performance: Lese-Endpunkte p95 < 100 ms, Queue-Mutationen p95 < 150 ms bei üblicher Last.
- [ ] Sicherheit: keine Secrets/Token in Logs; HTTPS; CSP; Rate-Limits; Argon2id; Token-Hashing.
