-- 0001_init.sql — Zukka Radio scheduler schema (Issue #7)
-- Source of truth: docs/consolidated-architecture.md §3 (+ §3.3, §3.4, §6.10.1)
-- Conventions (§3.1): one DB, STRICT tables, TEXT UUIDv7 PKs (app-generated),
-- RFC3339 UTC ms timestamps, snake_case TEXT+CHECK enums, JSON via json_valid.
-- Migration is append-only.

-- ---------------------------------------------------------------------------
-- roles: admin / operator / caster + JSON permission matrix
-- ---------------------------------------------------------------------------
CREATE TABLE roles (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE CHECK (name IN ('admin', 'operator', 'caster')),
    description TEXT NOT NULL DEFAULT '',
    permissions TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(permissions)),
    is_system   INTEGER NOT NULL DEFAULT 0 CHECK (is_system IN (0, 1)),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- casters: identity + stream credentials
-- ---------------------------------------------------------------------------
CREATE TABLE casters (
    id                   TEXT PRIMARY KEY,
    username             TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name         TEXT NOT NULL,
    email                TEXT UNIQUE COLLATE NOCASE,
    password_hash        TEXT NOT NULL,
    role_id              TEXT NOT NULL REFERENCES roles(id) ON DELETE RESTRICT,
    stream_username      TEXT UNIQUE COLLATE NOCASE,
    stream_password_hash TEXT,
    harbor_mount         TEXT UNIQUE,
    avatar_url           TEXT,
    bio                  TEXT NOT NULL DEFAULT '',
    is_active            INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- sessions: browser sessions (7d idle / 30d absolute)
-- ---------------------------------------------------------------------------
CREATE TABLE sessions (
    id                 TEXT PRIMARY KEY,
    caster_id          TEXT NOT NULL REFERENCES casters(id) ON DELETE CASCADE,
    token_hash         TEXT NOT NULL UNIQUE,
    csrf_secret        TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    last_seen_at       TEXT NOT NULL,
    idle_expires_at    TEXT NOT NULL,
    absolute_expires_at TEXT NOT NULL,
    ip_address         TEXT,
    user_agent         TEXT,
    revoked_at         TEXT,
    CHECK (idle_expires_at < absolute_expires_at)
) STRICT;

-- ---------------------------------------------------------------------------
-- shows: format + recurrence rule
-- ---------------------------------------------------------------------------
CREATE TABLE shows (
    id          TEXT PRIMARY KEY,
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    artwork_url TEXT,
    links       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(links)),
    recurrence  TEXT,
    caster_id   TEXT REFERENCES casters(id) ON DELETE SET NULL,
    created_by  TEXT NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- sources: mirror of the 6 priority layers (priority fixed, mirror of script)
-- ---------------------------------------------------------------------------
CREATE TABLE sources (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE CHECK (name IN ('live', 'show', 'queue', 'rotation', 'relay', 'fun')),
    kind       TEXT NOT NULL CHECK (kind IN ('live', 'show', 'queue', 'rotation', 'relay', 'fun')),
    priority   INTEGER NOT NULL CHECK (priority BETWEEN 1 AND 100),
    enabled    INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    config     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- queues: named lists per caster (version = optimistic lock)
-- ---------------------------------------------------------------------------
CREATE TABLE queues (
    id          TEXT PRIMARY KEY,
    caster_id   TEXT NOT NULL REFERENCES casters(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    kind        TEXT NOT NULL DEFAULT 'background' CHECK (kind IN ('background', 'slot')),
    version     INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (caster_id, name)
) STRICT;

-- ---------------------------------------------------------------------------
-- broadcasts: send slots, open end allowed, content unique (§3.3)
-- ---------------------------------------------------------------------------
CREATE TABLE broadcasts (
    id          TEXT PRIMARY KEY,
    show_id     TEXT REFERENCES shows(id) ON DELETE SET NULL,
    caster_id   TEXT NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    kind        TEXT NOT NULL CHECK (kind IN ('live', 'recording', 'playlist')),
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    starts_at   TEXT NOT NULL,
    ends_at     TEXT,
    status      TEXT NOT NULL DEFAULT 'scheduled'
                CHECK (status IN ('scheduled', 'live', 'ended', 'cancelled')),
    tape_id     TEXT REFERENCES tapes(id) ON DELETE SET NULL,
    queue_id    TEXT REFERENCES queues(id) ON DELETE SET NULL,
    version     INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by  TEXT NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    CHECK (ends_at IS NULL OR ends_at > starts_at),
    CHECK (
        (kind = 'live'      AND tape_id IS NULL     AND queue_id IS NULL) OR
        (kind = 'recording' AND tape_id IS NOT NULL AND queue_id IS NULL) OR
        (kind = 'playlist'  AND tape_id IS NULL     AND queue_id IS NOT NULL)
    )
) STRICT;

-- ---------------------------------------------------------------------------
-- tracks: local projection of the Music API
-- ---------------------------------------------------------------------------
CREATE TABLE tracks (
    id           TEXT PRIMARY KEY,
    music_api_id TEXT NOT NULL UNIQUE,
    isrc         TEXT UNIQUE,
    title        TEXT NOT NULL,
    artist       TEXT NOT NULL DEFAULT '',
    album        TEXT NOT NULL DEFAULT '',
    duration_ms  INTEGER NOT NULL CHECK (duration_ms >= 0),
    storage_key  TEXT NOT NULL,
    artwork_url  TEXT,
    visibility   TEXT NOT NULL DEFAULT 'shared' CHECK (visibility IN ('shared', 'private')),
    caster_id    TEXT REFERENCES casters(id) ON DELETE SET NULL,
    metadata     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- tapes: one table for recording and upload (§3.3)
-- ---------------------------------------------------------------------------
CREATE TABLE tapes (
    id            TEXT PRIMARY KEY,
    slug          TEXT NOT NULL UNIQUE,
    caster_id     TEXT NOT NULL REFERENCES casters(id) ON DELETE RESTRICT,
    show_id       TEXT REFERENCES shows(id) ON DELETE SET NULL,
    broadcast_id  TEXT REFERENCES broadcasts(id) ON DELETE SET NULL,
    title         TEXT,
    description   TEXT,
    started_at    TEXT NOT NULL,
    ended_at      TEXT NOT NULL,
    duration_ms   INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    source        TEXT NOT NULL CHECK (source IN ('live', 'upload')),
    music_api_id  TEXT UNIQUE,
    storage_key   TEXT,
    format        TEXT,
    mime          TEXT,
    size_bytes    INTEGER,
    etag          TEXT,
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'processing', 'ready', 'failed', 'deleted')),
    visible       INTEGER NOT NULL DEFAULT 0 CHECK (visible IN (0, 1)),
    published_at  TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    CHECK (ended_at >= started_at)
) STRICT;

-- ---------------------------------------------------------------------------
-- queue_items: track | tape | link, exactly one content field (§3.3)
-- ---------------------------------------------------------------------------
CREATE TABLE queue_items (
    id             TEXT PRIMARY KEY,
    queue_id       TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
    position       INTEGER NOT NULL CHECK (position >= 0),
    kind           TEXT NOT NULL CHECK (kind IN ('track', 'tape', 'link')),
    track_id       TEXT REFERENCES tracks(id) ON DELETE CASCADE,
    tape_id        TEXT REFERENCES tapes(id) ON DELETE CASCADE,
    link_url       TEXT,
    link_provider  TEXT CHECK (link_provider IN ('youtube', 'spotify', 'other')),
    link_state     TEXT CHECK (link_state IN ('pending', 'ready', 'failed', 'unplayable')),
    title_override TEXT,
    state          TEXT NOT NULL DEFAULT 'pending'
                   CHECK (state IN ('pending', 'playing', 'played', 'skipped')),
    played_at      TEXT,
    created_at     TEXT NOT NULL,
    UNIQUE (queue_id, position),
    CHECK (
        (kind = 'track' AND track_id IS NOT NULL AND tape_id IS NULL AND link_url IS NULL) OR
        (kind = 'tape'  AND tape_id  IS NOT NULL AND track_id IS NULL AND link_url IS NULL) OR
        (kind = 'link'  AND link_url IS NOT NULL AND track_id IS NULL AND tape_id IS NULL)
    )
) STRICT;

-- ---------------------------------------------------------------------------
-- waveforms: one format for tracks and tapes (§3.3)
-- ---------------------------------------------------------------------------
CREATE TABLE waveforms (
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('track', 'tape')),
    subject_id   TEXT NOT NULL,
    scheme       INTEGER NOT NULL,
    algorithm    TEXT NOT NULL,
    ms_per_bar   INTEGER NOT NULL,
    bars         INTEGER NOT NULL,
    data         BLOB NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (subject_kind, subject_id)
) STRICT;

-- ---------------------------------------------------------------------------
-- tape_jobs: waveform pipeline queue
-- ---------------------------------------------------------------------------
CREATE TABLE tape_jobs (
    id         TEXT PRIMARY KEY,
    tape_id    TEXT NOT NULL REFERENCES tapes(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('probe', 'peaks')),
    state      TEXT NOT NULL DEFAULT 'pending'
               CHECK (state IN ('pending', 'running', 'done', 'failed')),
    error      TEXT,
    attempts   INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (tape_id, kind)
) STRICT;

-- ---------------------------------------------------------------------------
-- upload_jobs: portal uploads, restart-safe
-- ---------------------------------------------------------------------------
CREATE TABLE upload_jobs (
    id         TEXT PRIMARY KEY,
    caster_id  TEXT REFERENCES casters(id) ON DELETE SET NULL,
    filename   TEXT NOT NULL,
    size_bytes INTEGER,
    sha256     TEXT,
    state      TEXT NOT NULL DEFAULT 'pending'
               CHECK (state IN ('pending', 'running', 'done', 'failed')),
    track_id   TEXT REFERENCES tracks(id) ON DELETE SET NULL,
    error      TEXT,
    attempts   INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- announcement_templates: global or per show
-- ---------------------------------------------------------------------------
CREATE TABLE announcement_templates (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    event      TEXT NOT NULL CHECK (event IN ('live_on', 'live_off', 'show_reminder',
                                              'recording_published', 'fallback_entered',
                                              'error', 'manual')),
    show_id    TEXT REFERENCES shows(id) ON DELETE CASCADE,
    channel    TEXT NOT NULL DEFAULT 'telegram' CHECK (channel IN ('telegram')),
    body       TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX uq_announcement_templates_global
    ON announcement_templates (event, channel) WHERE show_id IS NULL;
CREATE UNIQUE INDEX uq_announcement_templates_show
    ON announcement_templates (event, show_id, channel) WHERE show_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- announcements: send log + retry
-- ---------------------------------------------------------------------------
CREATE TABLE announcements (
    id              TEXT PRIMARY KEY,
    event           TEXT NOT NULL CHECK (event IN ('live_on', 'live_off', 'show_reminder',
                                                   'recording_published', 'fallback_entered',
                                                   'error', 'manual')),
    show_id         TEXT REFERENCES shows(id) ON DELETE SET NULL,
    broadcast_id    TEXT REFERENCES broadcasts(id) ON DELETE SET NULL,
    caster_id       TEXT REFERENCES casters(id) ON DELETE SET NULL,
    template_id     TEXT REFERENCES announcement_templates(id) ON DELETE SET NULL,
    channel         TEXT NOT NULL CHECK (channel IN ('telegram')),
    payload         TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
    dedupe_key      TEXT UNIQUE,
    scheduled_at    TEXT,
    sent_at         TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'sent', 'failed', 'cancelled')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TEXT,
    error           TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- event_log: append-only audit AND schedule log (no FKs, historical IDs)
-- ---------------------------------------------------------------------------
CREATE TABLE event_log (
    id              TEXT PRIMARY KEY,
    occurred_at     TEXT NOT NULL,
    event           TEXT NOT NULL CHECK (event IN (
                        'broadcast_created', 'broadcast_updated', 'broadcast_cancelled',
                        'broadcast_started', 'broadcast_ended', 'source_switched',
                        'live_connected', 'live_disconnected', 'live_override_set',
                        'live_override_cleared', 'fallback', 'error', 'dst_adjusted',
                        'announcement_sent', 'announcement_failed', 'caster_created',
                        'caster_updated', 'role_changed', 'tape_published',
                        'tape_unpublished', 'control_action')),
    actor_caster_id TEXT,
    broadcast_id    TEXT,
    caster_id       TEXT,
    source_layer    TEXT,
    details         TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(details)),
    created_at      TEXT NOT NULL
) STRICT;

CREATE TRIGGER event_log_no_update
BEFORE UPDATE ON event_log
BEGIN
    SELECT RAISE(ABORT, 'event_log is append-only');
END;

CREATE TRIGGER event_log_no_delete
BEFORE DELETE ON event_log
BEGIN
    SELECT RAISE(ABORT, 'event_log is append-only');
END;

-- ---------------------------------------------------------------------------
-- settings: key/value (e.g. queue.active_id)
-- ---------------------------------------------------------------------------
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- Indexes for FK / query columns
-- ---------------------------------------------------------------------------
CREATE INDEX idx_casters_role_id ON casters (role_id);
CREATE INDEX idx_sessions_caster_id ON sessions (caster_id);
CREATE INDEX idx_shows_caster_id ON shows (caster_id);
CREATE INDEX idx_shows_created_by ON shows (created_by);
CREATE INDEX idx_broadcasts_starts_at_status_show_id ON broadcasts (starts_at, status, show_id);
CREATE INDEX idx_broadcasts_caster_id ON broadcasts (caster_id);
CREATE INDEX idx_broadcasts_tape_id ON broadcasts (tape_id);
CREATE INDEX idx_broadcasts_queue_id ON broadcasts (queue_id);
CREATE INDEX idx_queues_caster_id ON queues (caster_id);
CREATE INDEX idx_queue_items_queue_id_position ON queue_items (queue_id, position);
CREATE INDEX idx_queue_items_track_id ON queue_items (track_id);
CREATE INDEX idx_queue_items_tape_id ON queue_items (tape_id);
CREATE INDEX idx_tracks_caster_id ON tracks (caster_id);
CREATE INDEX idx_tracks_visibility ON tracks (visibility);
CREATE INDEX idx_tapes_status_visible ON tapes (status, visible);
CREATE INDEX idx_tapes_caster_id ON tapes (caster_id);
CREATE INDEX idx_tapes_show_id ON tapes (show_id);
CREATE INDEX idx_tapes_broadcast_id ON tapes (broadcast_id);
CREATE INDEX idx_tape_jobs_state ON tape_jobs (state);
CREATE INDEX idx_upload_jobs_state ON upload_jobs (state);
CREATE INDEX idx_upload_jobs_caster_id ON upload_jobs (caster_id);
CREATE INDEX idx_upload_jobs_track_id ON upload_jobs (track_id);
CREATE INDEX idx_announcements_status ON announcements (status);
CREATE INDEX idx_announcements_scheduled_at ON announcements (scheduled_at);
CREATE INDEX idx_announcements_broadcast_id ON announcements (broadcast_id);
CREATE INDEX idx_event_log_occurred_at_event ON event_log (occurred_at, event);
CREATE INDEX idx_event_log_broadcast_id ON event_log (broadcast_id);
CREATE INDEX idx_event_log_caster_id ON event_log (caster_id);

-- ---------------------------------------------------------------------------
-- Seed: roles (permission matrix §6.10.1) + sources (6 layers)
-- IDs are fixed UUIDv7 literals (SQL cannot generate UUIDv7).
-- ---------------------------------------------------------------------------
INSERT INTO roles (id, name, description, permissions, is_system, created_at, updated_at) VALUES
('0192f0a1-0001-7000-8000-000000000001', 'admin', 'Full access', '{"library.read":true,"library.upload":true,"library.delete":true,"queue.write.own":true,"queue.write.any":true,"show.write":true,"broadcast.write":true,"control.read":true,"control.write":true,"announcement.write":true,"tape.publish":true,"caster.manage":true,"settings.manage":true,"event_log.read":true}', 1, '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0001-7000-8000-000000000002', 'operator', 'Operational access', '{"library.read":true,"library.upload":true,"library.delete":true,"queue.write.own":true,"queue.write.any":true,"show.write":true,"broadcast.write":true,"control.read":true,"control.write":true,"announcement.write":true,"tape.publish":true,"caster.manage":true,"settings.manage":false,"event_log.read":true}', 1, '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0001-7000-8000-000000000003', 'caster', 'Own show and library', '{"library.read":true,"library.upload":true,"library.delete":false,"queue.write.own":true,"queue.write.any":false,"show.write":true,"broadcast.write":true,"control.read":true,"control.write":false,"announcement.write":true,"tape.publish":true,"caster.manage":false,"settings.manage":false,"event_log.read":false}', 1, '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z')
ON CONFLICT DO NOTHING;

INSERT INTO sources (id, name, kind, priority, enabled, config, created_at, updated_at) VALUES
('0192f0a1-0002-7000-8000-000000000001', 'live',     'live',     100, 1, '{}', '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0002-7000-8000-000000000002', 'show',     'show',      80, 1, '{}', '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0002-7000-8000-000000000003', 'queue',    'queue',     60, 1, '{}', '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0002-7000-8000-000000000004', 'rotation', 'rotation',  40, 1, '{}', '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0002-7000-8000-000000000005', 'relay',    'relay',     20, 1, '{}', '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z'),
('0192f0a1-0002-7000-8000-000000000006', 'fun',      'fun',        1, 1, '{}', '2026-11-18T00:00:00.000Z', '2026-11-18T00:00:00.000Z')
ON CONFLICT DO NOTHING;
