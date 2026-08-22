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
     │  OFF → ⚫ offline │      │  postprocess.py (#4)    │
     │  (instant)       │      │  Upload to AzuraCast    │
     └─────────────────┘      │  Upload to Funkwhale    │
                              │  Post 📼 recording link  │
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
| `postprocess.py`       | module (loaded by watcher)  | After finalization, pre-upload | Optional ffmpeg master chain: loudness + MP3/Opus (issue #4, default OFF)         |
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
- **No secrets in code or systemd units.** All credentials (AzuraCast API key,
  Telegram bot token/channel, Funkwhale admin password) come from the environment
  via `EnvironmentFile` — never inline in unit files, never hardcoded in scripts.
- **Deploy as EnvironmentFile:** copy `.env` to the server as `/etc/zukka.env`
  (`chmod 600`, owned by root) and reference it from every unit:

  ```ini
  EnvironmentFile=/etc/zukka.env
  ```

  systemd `EnvironmentFile` syntax is plain `KEY=value` lines (no `export`, no quotes).
- State files (`*.json`) are git-ignored — they're server runtime data, not source.

## Development

```bash
cd zukka_radio
pip install -r requirements.txt   # only `mutagen`
python3 test_logic.py             # no-deps test runner (incl. postprocess tests)
# or: python3 -m pytest test_logic.py -v
```

### Tests

`test_logic.py` covers the pure logic that historically caused production bugs:

- UTC → Berlin timezone conversion
- title / time-range formatting
- filename prefixing
- album title / display name mapping (`mctoastus` → `duck`)
- postprocess.py (issue #4): ffmpeg arg/filter-chain building, loudness gain
  math, ebur128 parsing, enable/disable + graceful-failure logic — all pure,
  **no audio binaries required** (ffmpeg calls are injected fakes in tests)

## Post-Processing (issue #4)

`postprocess.py` is a standalone, modular ffmpeg master chain for recordings,
called by `recording_watcher.py` after a broadcast is finalized and **before**
upload to AzuraCast/Funkwhale. It is **disabled by default** — the production
behavior is byte-identical until `POSTPROCESS_ENABLED=true` is deployed.

**Chain** (SOTA order, every stage optional):

1. `highpass=f=30` — rumble/DC removal
2. optional `acompressor` (`POSTPROCESS_COMPRESSOR=true`)
3. static loudness normalization — EBU R128 measured once, then a **constant**
   gain (no dynamic `loudnorm`, which pumps on music). Target `TARGET_LUFS=-16`,
   true peak capped at `TARGET_TP=-1.5` dBTP (gain = min of both corrections).
4. `alimiter` safety limiter (auto-level off — pure brickwall at TARGET_TP)
5. optional `afade` in/out (`POSTPROCESS_FADE_IN` / `POSTPROCESS_FADE_OUT`, s)

**Output:** MP3 320 kbps CBR (`libmp3lame -b:a 320k`). CBR + Xing header
(written automatically by ffmpeg ≥ 4.0) → correct duration, perfect seeking,
Safari support. Opus optional via `FORMAT=opus`.

**Config (env, read at call time):**

| Env                         | Default      | Meaning                                  |
| --------------------------- | ------------ | ---------------------------------------- |
| `POSTPROCESS_ENABLED`       | `false`      | master switch — module is inert without it |
| `TARGET_LUFS`               | `-16`        | loudness target (LUFS)                   |
| `TARGET_TP`                 | `-1.5`       | true-peak ceiling (dBTP)                 |
| `FORMAT`                    | `mp3`        | `mp3` or `opus`                          |
| `BITRATE`                   | `320k`/`128k`| CBR bitrate (per format)                 |
| `POSTPROCESS_COMPRESSOR`    | `false`      | compressor stage                         |
| `POSTPROCESS_FADE_IN`       | `0`          | fade-in seconds (0 = off)                |
| `POSTPROCESS_FADE_OUT`      | `0`          | fade-out seconds (0 = off)               |
| `POSTPROCESS_KEEP_ORIGINAL` | `true`       | `false` → remove raw recording after success |
| `POSTPROCESS_OUTPUT_DIR`    | next to src  | where the master is written              |

**Design:** pure functions (`load_config`, `compute_gain`, `build_filter_chain`,
`build_ffmpeg_args`, `output_path`, `parse_ebur128_output`) build the ffmpeg
command line; a thin execution layer (`measure_loudness`, `probe_duration`,
`process`) runs it. `process(src) -> dst` is always graceful: any failure
(missing ffmpeg, measurement error, encode error) logs and returns `None`,
so the watcher keeps using the original recording. Requires `ffmpeg`/`ffprobe`
on the server (no pip dependency).

## Deployment

Scripts are deployed to `cast.zukkafabrik.de` via `scp` and run under systemd.

```bash
# pipeline.py
scp pipeline.py cast.zukkafabrik.de:/tmp/ && \
  ssh cast.zukkafabrik.de "sudo cp /tmp/pipeline.py /srv/azuracast/pipeline.py && sudo systemctl restart zukka-webhook"

# recording_watcher.py (+ postprocess.py, issue #4)
scp recording_watcher.py postprocess.py cast.zukkafabrik.de:/tmp/ && \
  ssh cast.zukkafabrik.de "sudo cp /tmp/recording_watcher.py /tmp/postprocess.py /srv/azuracast/ && \
    sudo systemctl restart zukka-recording-watcher.timer"
```

> ⚠️ Post-processing stays **off** after deploy unless `POSTPROCESS_ENABLED=true`
> is added to `/etc/zukka.env` (same `EnvironmentFile` as the watcher).

### systemd units (on cast.zukkafabrik.de)

All units load secrets from `/etc/zukka.env` (copied from `.env`, chmod 600) —
**no inline secrets in unit files**. `StandardOutput=journal` is the default for
services and captures script stdout; scripts also flush their stdout per line so
`journalctl` always shows output immediately.

| Unit                            | Script               | Env                                           |
| ------------------------------- | -------------------- | --------------------------------------------- |
| `zukka-webhook.service`         | pipeline.py          | `EnvironmentFile=/etc/zukka.env`              |
| `zukka-recording-watcher.timer` | recording_watcher.py | every 2 min, `EnvironmentFile=/etc/zukka.env` |
| `zukka-age-broadcasts.timer`    | age_broadcasts.py    | daily 3am, `EnvironmentFile=/etc/zukka.env`   |
| `zukka-healthcheck.timer`       | curl /healthz        | every 5 min (see below)                       |

Example — `zukka-webhook.service`:

```ini
[Unit]
Description=radio zukka live webhook notifier
After=network-online.target

[Service]
ExecStart=/usr/bin/python3 /srv/azuracast/pipeline.py
EnvironmentFile=/etc/zukka.env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Example — healthcheck (oneshot + timer):

```ini
# /etc/systemd/system/zukka-healthcheck.service
[Unit]
Description=radio zukka webhook healthcheck

[Service]
Type=oneshot
ExecStart=/usr/bin/curl -fsS http://127.0.0.1:5099/healthz
```

```ini
# /etc/systemd/system/zukka-healthcheck.timer
[Unit]
Description=radio zukka webhook healthcheck (every 5 min)

[Timer]
OnBootSec=1min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
```

A failing healthcheck shows up as a failed `zukka-healthcheck.service` in
`systemctl list-timers` / `journalctl`; wire it into the existing
`mellon-alertmanager-telegram` alerting (e.g. Prometheus blackbox probe or a
watchdog on the oneshot failure) to get a Telegram alert when the webhook is down.

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
# Healthcheck (pipeline.py serves GET /healthz → 200 {"status": "ok"})
ssh cast.zukkafabrik.de "curl -fsS http://127.0.0.1:5099/healthz"

# Logs — journald captures script stdout (line-buffered, see systemd section)
ssh cast.zukkafabrik.de "sudo journalctl -u zukka-webhook -f"          # ON/OFF
ssh cast.zukkafabrik.de "sudo journalctl -u zukka-recording-watcher -f" # uploads
ssh cast.zukkafabrik.de "sudo systemctl start zukka-recording-watcher"  # manual trigger
```

If `journalctl -u zukka-webhook` shows no entries while the service is
`active/running`, check that the service actually runs the current script
(restart after deploy) and that no firewall rule blocks AzuraCast → port 5099
(see ISSUES.md #1 — `ufw allow from 172.24.0.0/16 to any port 5099 proto tcp`).

---

## Obsolete Scripts (do NOT deploy)

| Script                 | Status   | Superseded by          | Location                      |
| ---------------------- | -------- | ---------------------- | ----------------------------- |
| `webhook_listener.py`  | obsolete | `pipeline.py`          | `legacy/` (marked OBSOLETE)   |
| `telegram_notifier.py` | obsolete | `pipeline.py`          | `legacy/` (marked OBSOLETE)   |
| `auto_upload.py`       | obsolete | `recording_watcher.py` | `legacy/` (marked OBSOLETE)   |
| `app.py`               | obsolete | `recording_watcher.py` | `legacy/` (marked OBSOLETE)   |

Moved to `legacy/` and marked with an `OBSOLETE — DO NOT DEPLOY` banner; kept
for reference only. On the server they should be removed — they are still
present in `/srv/azuracast/` and nothing references them anymore.

## Station Config (see STATION.md)

Rotation weights, playlist structure, and API recipes for querying/updating AzuraCast config live in `STATION.md`.
