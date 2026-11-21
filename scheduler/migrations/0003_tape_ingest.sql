-- 0003_tape_ingest.sql — Issue #132
--
-- The ingest pipeline reuses `tape_jobs` for the upload step: a staged MP3 is
-- uploaded to the music API and the job records retries exactly like the
-- waveform jobs. SQLite cannot alter a CHECK constraint in place, so the table
-- is rebuilt. No other table references `tape_jobs` (all foreign keys point
-- outward to `tapes`), so a rebuild without toggling `PRAGMA foreign_keys` is
-- safe. `sqlx::migrate!` wraps this file in one transaction.

CREATE TABLE tape_jobs_new (
    id         TEXT PRIMARY KEY,
    tape_id    TEXT NOT NULL REFERENCES tapes(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('probe', 'peaks', 'upload')),
    state      TEXT NOT NULL DEFAULT 'pending'
               CHECK (state IN ('pending', 'running', 'done', 'failed')),
    error      TEXT,
    attempts   INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (tape_id, kind)
) STRICT;

INSERT INTO tape_jobs_new (id, tape_id, kind, state, error, attempts, created_at, updated_at)
    SELECT id, tape_id, kind, state, error, attempts, created_at, updated_at
    FROM tape_jobs;

DROP TABLE tape_jobs;

ALTER TABLE tape_jobs_new RENAME TO tape_jobs;

-- Index from 0001 was dropped with the old table.
CREATE INDEX idx_tape_jobs_state ON tape_jobs (state);