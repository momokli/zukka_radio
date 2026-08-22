# Legacy Scripts

These are superseded by the current automation. Kept for reference only — do NOT deploy.
Each file carries an `OBSOLETE — DO NOT DEPLOY` banner; none of them are part of the active pipeline.

| Script | Superseded by | Notes |
|--------|---------------|-------|
| `webhook_listener.py` | `pipeline.py` | Old post-cast handler (upload + notify in one) |
| `telegram_notifier.py` | `pipeline.py` | Polling Telegram announcer (backup) |
| `auto_upload.py` | `recording_watcher.py` | Cron-based batch uploader |
| `app.py` (+ `templates/`, `static/`) | `recording_watcher.py` | Flask web UI for manual review |

> **Server cleanup:** these files are also present in `/srv/azuracast/` on
> `cast.zukkafabrik.de` and can be deleted there — nothing references them anymore.

The current live scripts are in the repo root:
- `pipeline.py` — webhook notifier (ON/OFF Telegram)
- `recording_watcher.py` — recording processor (upload + 📼 post)
- `age_broadcasts.py` — daily rotation aging
