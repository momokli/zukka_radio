# radio zukka — Automation

> Community radio automation: DJ connect/disconnect → Telegram notifications + recording upload to AzuraCast & Funkwhale.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                      AzuraCast Webhooks                       │
│  zukka pipeline (#3): triggers live_connect + live_disconnect│
│  → POST http://172.24.0.1:5099                               │
└──────────────────────────┬───────────────────────────────────┘
                           │
              ┌────────────┴────────────┐
              ▼                         ▼
     ┌─────────────────┐      ┌─────────────────────────┐
     │  pipeline.py     │      │  recording_watcher.py   │
     │  (systemd, dauer)│      │  (systemd timer, 2 min) │
     │                  │      │                          │
     │  ON  → 🔴 live   │      │  Find finalized recs    │
     │  OFF → ⚫ offline │      │  Upload to AzuraCast    │
     │  (instant)       │      │  Upload to Funkwhale    │
     └─────────────────┘      │  Post 📼 recording link  │
                              └─────────────────────────┘

     ┌─────────────────┐
     │ age_broadcasts  │ (systemd timer, daily 3am)
     │ Moves tracks >30d from DJ-fresh → shared pool
     └─────────────────┘
```

## Components

| Component              | Type                        | When                        | What                                                                                |
| ---------------------- | --------------------------- | --------------------------- | ----------------------------------------------------------------------------------- |
| `pipeline.py`          | systemd service (always on) | Webhook-triggered (instant) | 🔴 ON / ⚫ OFF Telegram messages                                                    |
| `recording_watcher.py` | systemd timer (every 2 min) | Periodic                    | Finds finalized recordings, uploads to AzuraCast + Funkwhale, posts 📼 link         |
| `age_broadcasts.py`    | systemd timer (daily 3am)   | Daily                       | Moves tracks older than 30 days from per-DJ fresh playlists to shared rotation pool |

## Naming Convention

| Field     | Format                                  | Example                                |
| --------- | --------------------------------------- | -------------------------------------- |
| Title     | `{display_name} — DD.MM.YY HH:MM`       | `duck — 23.06.26 20:30`                |
| Album     | `{display_name} @ radio zukka`          | `duck @ radio zukka`                   |
| Artist    | `{display_name}`                        | `duck` (never `mctoastus`)             |
| Filename  | `{username}_stream_YYYYMMDD-HHMMSS.ogg` | `mctoastus_stream_20260623-203021.ogg` |
| Copyright | `(c) YYYY radio zukka`                  | `(c) 2026 radio zukka`                 |
| License   | `cc-by-nc-sa-4.0`                       | (Funkwhale license code)               |

## Telegram Message Format

**ON (pipeline.py, instant):**

```
🔴 momo ist jetzt live auf radio zukka 🎧

🌐 web player
radio.zukkafabrik.de
```

**OFF (pipeline.py, instant):**

```
⚫ momo war live auf radio zukka 🎧
```

**Recording (recording_watcher.py, delayed ≤2 min):**

```
📼 momo — 23.06.26 · 20:30 – 20:33 · 2 min

recording anhören
music.zukkafabrik.de/library/tracks/XXX
```

## DJs

| Display | Username  | AzuraCast ID | Playlist ID               |
| ------- | --------- | ------------ | ------------------------- |
| ANKD    | hovio     | 2            | 9 (broadcasts-ankd)       |
| duck    | mctoastus | 3            | 10 (broadcasts-mctoastus) |
| momo    | momo      | 1            | 11 (broadcasts-momo)      |

## State Files (on cast.zukkafabrik.de)

| File                                             | Used by              | Content                                       |
| ------------------------------------------------ | -------------------- | --------------------------------------------- |
| `/srv/azuracast/recording_state.json`            | recording_watcher.py | `{uploaded: {bid: {...}}, started_at: "ISO"}` |
| `/opt/http/radio.zukkafabrik.de/live_state.json` | pipeline.py          | `{live: bool, streamer: "name"}`              |

## Server Paths

| Path                                                                           | Content                            |
| ------------------------------------------------------------------------------ | ---------------------------------- |
| `/srv/azuracast/pipeline.py`                                                   | Live/DJ notifier (webhook handler) |
| `/srv/azuracast/recording_watcher.py`                                          | Recording processor                |
| `/srv/azuracast/age_broadcasts.py`                                             | Rotation aging                     |
| `/var/lib/docker/volumes/azuracast_station_data/_data/zukka_radio/recordings/` | Raw streamer recordings            |
| `/srv/funkwhale/data/music/{dj}/`                                              | Funkwhale music files              |
| `/opt/http/radio.zukkafabrik.de/`                                              | Landing page + live_state + covers |

## Key Instances

| Service                | URL                                    | Auth                                |
| ---------------------- | -------------------------------------- | ----------------------------------- |
| AzuraCast              | `https://cast.zukkafabrik.de`          | API key in `.env`                   |
| Funkwhale              | `https://music.zukkafabrik.de`         | admin / password in `.env`          |
| Funkwhale Library UUID | `3338514d-4a10-4ae3-a017-5c90deac398c` | —                                   |
| Landing page           | `https://radio.zukkafabrik.de`         | static files on cast.zukkafabrik.de |

---

## Security

- **`.env` is git-ignored.** Copy `.env.example` → `.env` and fill in real values.
- **No secrets in code.** `recording_watcher.py` still has the Funkwhale admin password hardcoded as a Base64 header (`FW_AUTH`). TODO: move to env var.
- State files (`*.json`) are git-ignored — they're server runtime data, not source.

## Development

```bash
cd zukka_radio
pip install -r requirements.txt   # only `mutagen`
python3 test_logic.py             # no-deps test runner (6 tests)
# or: python3 -m pytest test_logic.py -v
```

### Tests

`test_logic.py` covers the pure logic that historically caused production bugs:

- UTC → Berlin timezone conversion
- title / time-range formatting
- filename prefixing
- album title / display name mapping (`mctoastus` → `duck`)

## Deployment

Scripts are deployed to `cast.zukkafabrik.de` via `scp` and run under systemd.

```bash
# pipeline.py
scp pipeline.py cast.zukkafabrik.de:/tmp/ && \
  ssh cast.zukkafabrik.de "sudo cp /tmp/pipeline.py /srv/azuracast/pipeline.py && sudo systemctl restart zukka-webhook"

# recording_watcher.py
scp recording_watcher.py cast.zukkafabrik.de:/tmp/ && \
  ssh cast.zukkafabrik.de "sudo cp /tmp/recording_watcher.py /srv/azuracast/recording_watcher.py"
```

### systemd units (on cast.zukkafabrik.de)

| Unit                            | Script               | Env                                           |
| ------------------------------- | -------------------- | --------------------------------------------- |
| `zukka-webhook.service`         | pipeline.py          | `TELEGRAM_CHANNEL`, `ZUKKA_WEBHOOK_PORT=5099` |
| `zukka-recording-watcher.timer` | recording_watcher.py | every 2 min, `TELEGRAM_CHANNEL`               |
| `zukka-age-broadcasts.timer`    | age_broadcasts.py    | daily 3am                                     |

### DEV → PROD switch (Telegram channel)

```bash
ssh cast.zukkafabrik.de "sudo sed -i 's/TELEGRAM_CHANNEL=-1003560012041/TELEGRAM_CHANNEL=@zukka_radio/' \
  /etc/systemd/system/zukka-webhook.service \
  /etc/systemd/system/zukka-recording-watcher.service && \
  sudo systemctl daemon-reload && \
  sudo systemctl restart zukka-webhook zukka-recording-watcher.timer"
```

### Monitoring

```bash
ssh cast.zukkafabrik.de "sudo journalctl -u zukka-webhook -f"          # ON/OFF
ssh cast.zukkafabrik.de "sudo journalctl -u zukka-recording-watcher -f" # uploads
ssh cast.zukkafabrik.de "sudo systemctl start zukka-recording-watcher"  # manual trigger
```

---

## Obsolete Scripts (do NOT deploy)

| Script                 | Status   | Superseded by          |
| ---------------------- | -------- | ---------------------- |
| `webhook_listener.py`  | obsolete | `pipeline.py`          |
| `telegram_notifier.py` | obsolete | `pipeline.py`          |
| `auto_upload.py`       | obsolete | `recording_watcher.py` |
| `app.py`               | obsolete | `recording_watcher.py` |

These are kept for reference but are no longer active. On the server they should be removed (still present in `/srv/azuracast/`).

## Station Config (see STATION.md)

Rotation weights, playlist structure, and API recipes for querying/updating AzuraCast config live in `STATION.md`.
