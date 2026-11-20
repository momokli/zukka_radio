-- Per-show announcement timing overrides (Issue #36).
-- A row exists only when the show overrides at least one timing.
-- NULL in a column means "inherit the global value".
CREATE TABLE show_announcement_timings (
    show_id                    TEXT PRIMARY KEY REFERENCES shows(id) ON DELETE CASCADE,
    show_reminder_lead_seconds INTEGER
        CHECK (show_reminder_lead_seconds IS NULL
               OR show_reminder_lead_seconds BETWEEN 0 AND 604800),
    live_on_delay_seconds      INTEGER
        CHECK (live_on_delay_seconds IS NULL
               OR live_on_delay_seconds BETWEEN 0 AND 604800),
    live_off_delay_seconds     INTEGER
        CHECK (live_off_delay_seconds IS NULL
               OR live_off_delay_seconds BETWEEN 0 AND 604800),
    created_at                 TEXT NOT NULL,
    updated_at                 TEXT NOT NULL,
    CHECK (show_reminder_lead_seconds IS NOT NULL
        OR live_on_delay_seconds IS NOT NULL
        OR live_off_delay_seconds IS NOT NULL)
) STRICT;
