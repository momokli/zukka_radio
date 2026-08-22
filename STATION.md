# Zukka Radio — Station Configuration

> Last checked: 2026-05-11

## AutoDJ Rotation

Two-tier system: **DJ-fresh** pools (first 30 days, equal per-DJ weight) feed into a shared **rotation** pool after aging out.

```
  ANKD upload   ──▶  broadcasts-ankd      (weight 5, 30-day fresh)
  mctoastus     ──▶  broadcasts-mctoastus  (weight 5, 30-day fresh)
  momo          ──▶  broadcasts-momo       (weight 5, 30-day fresh)
                          │
                          │ after 30 days (daily cron)
                          ▼
                     broadcasts            (weight 20, rotation pool)
```

All broadcast playlists use `order: shuffle` and `avoid_duplicates: true`.

### Playlists

| #   | Name                     | Weight | Chance | Songs | Role                                      |
| --- | ------------------------ | ------ | ------ | ----- | ----------------------------------------- |
| 9   | **broadcasts-ankd**      | **5**  | 8.2%   | 9     | ANKD fresh (≤30 days)                     |
| 10  | **broadcasts-mctoastus** | **5**  | 8.2%   | 15    | mctoastus fresh (≤30 days)                |
| 11  | **broadcasts-momo**      | **5**  | 8.2%   | 3     | momo fresh (≤30 days)                     |
| 6   | broadcasts               | 20     | 32.8%  | 0     | Aged-out rotation pool                    |
| 8   | tati30-sets              | 15     | 24.6%  | 6     | Tati30 event recordings                   |
| 4   | fallback-sets            | 10     | 16.4%  | 54    | External DJ sets (Fusion, Sisyphos, etc.) |
| 1   | fallback                 | 1      | 1.6%   | 1     | "Technical difficulties" standby          |

| Total weight | 61  |
| ------------ | --- |

### How the two-tier system works

1. **New upload** → goes into the DJ's fresh playlist (weight 5 each)
2. **For 30 days** → each DJ gets exactly 8.2% of total airtime, regardless of how many sets they have
3. **Daily at 3am** → `age_broadcasts.py` moves tracks older than 30 days from DJ-fresh to the shared `broadcasts` pool (weight 20)
4. **In the rotation pool** → all DJs compete equally, older tracks play alongside other aged-out content

This prevents any single DJ from flooding the system while giving new sets guaranteed fresh airtime.

### Old vs New rotation

|                  | Old                           | New                                            |
| ---------------- | ----------------------------- | ---------------------------------------------- |
| Per-DJ fairness  | No — momo (30 sets) dominated | Yes — each DJ gets equal share                 |
| Freshness bias   | None                          | New sets get dedicated airtime for 30 days     |
| Flood protection | None                          | No DJ can exceed 8.2% total airtime            |
| Maintenance      | Zero                          | Daily cron (automatic via `age_broadcasts.py`) |

### Streamers

| DJ        | Username  | Active | Schedule | Playlist ID |
| --------- | --------- | ------ | -------- | ----------- |
| ANKD      | hovio     | ✓      | none     | 9           |
| mctoastus | mctoastus | ✓      | none     | 10          |
| momo      | momo      | ✓      | none     | 11          |
| Rookie361 | Rookie361 | ✓      | none     | —           |

No schedule enforcement — any streamer can connect anytime.

### Station

| Setting     | Value                                                |
| ----------- | ---------------------------------------------------- |
| Name        | radio zukka                                          |
| Shortcode   | `radio`                                              |
| Frontend    | Icecast                                              |
| Backend     | Liquidsoap                                           |
| Format      | 192kbps MP3                                          |
| Timezone    | Europe/Berlin                                        |
| Public page | on                                                   |
| Requests    | on                                                   |
| Listen URL  | `https://cast.zukkafabrik.de/listen/radio/radio.mp3` |

---

## Systemd Timers (on cast.zukkafabrik.de)

| Timer                        | Runs          | Does                                                          |
| ---------------------------- | ------------- | ------------------------------------------------------------- |
| `zukka-auto-upload.timer`    | Every 15 min  | Auto-uploads new sets ≥ 15 min to per-DJ fresh playlists      |
| `zukka-age-broadcasts.timer` | Daily at ~3am | Moves tracks older than 30 days from DJ-fresh → rotation pool |

```bash
# Monitor auto-uploader
ssh cast.zukkafabrik.de "sudo journalctl -u zukka-auto-upload.service -f"

# Monitor aging
ssh cast.zukkafabrik.de "sudo journalctl -u zukka-age-broadcasts.service -f"

# Trigger manually
ssh cast.zukkafabrik.de "sudo systemctl start zukka-auto-upload.service"
ssh cast.zukkafabrik.de "sudo systemctl start zukka-age-broadcasts.service"
```

---

## How to Retrieve Config via API

```bash
API_KEY="your-key-here"
HOST="https://cast.zukkafabrik.de"
```

### List all playlists with weights

```bash
curl -s -H "X-API-Key: $API_KEY" "$HOST/api/station/1/playlists" \
  | python3 -c "
import sys, json
for p in json.load(sys.stdin):
    print(f'id={p[\"id\"]:2}  weight={p[\"weight\"]:3}  songs={p[\"num_songs\"]:3}  {p[\"name\"]}')
"
```

### Get/list/update a single playlist

```bash
curl -s -H "X-API-Key: $API_KEY" "$HOST/api/station/1/playlist/6" | python3 -m json.tool

# Change weight
curl -s -X PUT -H "X-API-Key: $API_KEY" -H "Content-Type: application/json" \
  -d '{"weight": 25}' "$HOST/api/station/1/playlist/6"

# Enable/disable
curl -s -X PUT -H "X-API-Key: $API_KEY" -H "Content-Type: application/json" \
  -d '{"is_enabled": false}' "$HOST/api/station/1/playlist/6"

# Change order (shuffle, sequential, random)
curl -s -X PUT -H "X-API-Key: $API_KEY" -H "Content-Type: application/json" \
  -d '{"order": "shuffle"}' "$HOST/api/station/1/playlist/6"
```

### List streamers

```bash
curl -s -H "X-API-Key: $API_KEY" "$HOST/api/station/1/streamers" \
  | python3 -c "
import sys, json
for s in json.load(sys.stdin):
    print(f'{s[\"display_name\"]:15} active={s[\"is_active\"]}  broadcasts={len(s[\"broadcasts\"])}')
"
```

### Get station profile

```bash
curl -s -H "X-API-Key: $API_KEY" "$HOST/api/station/1/profile" | python3 -m json.tool
```

---

## Filesystem Layout (on server)

```
/var/lib/docker/volumes/azuracast_station_data/_data/zukka_radio/
├── recordings/           # raw streamer recordings
│   ├── hovio/            # ANKD
│   ├── mctoastus/
│   └── momo/
├── media/                # files available for rotation
│   ├── ankd_stream_*.ogg
│   ├── mctoastus_stream_*.ogg
│   ├── momo_stream_*.ogg
│   └── (external sets).m4a/.mp3
└── playlists/            # playlist definition files
```

Container-internal paths are the same under `/var/azuracast/stations/zukka_radio/`.
