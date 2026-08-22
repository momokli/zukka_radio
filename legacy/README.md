# Legacy Scripts

These are superseded by the current automation. Kept for reference only — do NOT deploy.

| Script | Superseded by | Notes |
|--------|---------------|-------|
| `webhook_listener.py` | `pipeline.py` | Old post-cast handler (upload + notify in one) |
| `telegram_notifier.py` | `pipeline.py` | Polling Telegram announcer (backup) |
| `auto_upload.py` | `recording_watcher.py` | Cron-based batch uploader |
| `app.py` (+ `templates/`, `static/`) | `recording_watcher.py` | Flask web UI for manual review |

The current live scripts are in the repo root:
- `pipeline.py` — webhook notifier (ON/OFF Telegram)
- `recording_watcher.py` — recording processor (upload + 📼 post)
- `age_broadcasts.py` — daily rotation aging
