#!/usr/bin/env python3
# ⚠️ OBSOLETE — DO NOT DEPLOY ⚠️
# Superseded by the current automation (see legacy/README.md and AGENT.md).
# Kept for reference only. Removed from production; not part of the
# active pipeline. Do not copy back to /srv/azuracast/.
"""
Zukka Radio — Telegram Broadcast Announcer
Posts a retro radio-style message when a DJ finishes casting,
with a link to listen on Funkwhale.
"""

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY", "")
STATION_ID = int(os.getenv("ZUKKA_STATION_ID", "1"))
TELEGRAM_TOKEN = os.getenv("TELEGRAM_BOT_API_KEY", "")
TELEGRAM_CHANNEL = os.getenv("TELEGRAM_CHANNEL", "@zukka_radio")
STATE_FILE = Path(os.getenv("ZUKKA_TELEGRAM_STATE", "telegram_state.json"))
MIN_DURATION_MINUTES = 2

FW_HOST = "https://music.zukkafabrik.de"
FW_LIBRARY = "3338514d-4a10-4ae3-a017-5c90deac398c"
FW_AUTH = base64.b64encode(f"admin:{os.getenv("FUNKWHALE_PASSWORD", "")}".encode()).decode()

if not API_KEY:
    print("ERROR: ZUKKA_RADIO_API_KEY not set", file=sys.stderr)
    sys.exit(1)
if not TELEGRAM_TOKEN:
    print("ERROR: TELEGRAM_BOT_API_KEY not set", file=sys.stderr)
    sys.exit(1)

HEADERS = {"X-API-Key": API_KEY, "Accept": "application/json"}


def api_get(path):
    req = urllib.request.Request(f"{HOST}/api{path}", headers=HEADERS)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def load_state():
    if STATE_FILE.exists():
        return set(json.loads(STATE_FILE.read_text()).get("announced", []))
    return set()


def save_state(s):
    STATE_FILE.write_text(json.dumps({"announced": sorted(s)}, indent=2))


def find_funkwhale_url(recording_path, dname):
    stem = Path(recording_path).stem
    hdrs = {"Authorization": f"Basic {FW_AUTH}"}
    try:
        q = urllib.parse.quote(stem)
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/tracks/?library={FW_LIBRARY}&page_size=5&q={q}",
            headers=hdrs,
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            results = json.loads(r.read()).get("results", [])
            if results:
                return f"{FW_HOST}/library/tracks/{results[0]['id']}"
    except:
        pass
    try:
        artist = dname.lower()
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/tracks/?library={FW_LIBRARY}&page_size=3&artist={artist}&ordering=-creation_date",
            headers=hdrs,
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            results = json.loads(r.read()).get("results", [])
            if results:
                return f"{FW_HOST}/library/tracks/{results[0]['id']}"
    except:
        pass
    return None


def tg_send(text):
    body = json.dumps(
        {
            "chat_id": TELEGRAM_CHANNEL,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": False,
        }
    ).encode()
    req = urllib.request.Request(
        f"https://api.telegram.org/bot{TELEGRAM_TOKEN}/sendMessage",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.loads(r.read()).get("ok", False)


def dur(ts_s, ts_e):
    if not ts_s or not ts_e:
        return 0, "?"
    try:
        t1 = datetime.fromisoformat(ts_s.replace("Z", "+00:00"))
        t2 = datetime.fromisoformat(ts_e.replace("Z", "+00:00"))
        s = int((t2 - t1).total_seconds())
        if s <= 0:
            return 0, "0min"
        h, m = divmod(s, 3600)
        m, sec = divmod(m, 60)
        if h > 0:
            return s, f"{h}h {m}min"
        return s, f"{m}min"
    except:
        return 0, "?"


def title(dname, ts_s):
    if ts_s:
        try:
            dt = datetime.fromisoformat(ts_s.replace("Z", "+00:00"))
            return f"{dname} — {dt.strftime('%d.%m.%y %H:%M')}"
        except:
            pass
    return dname


def main():
    print(f"[{datetime.now().strftime('%H:%M:%S')}] Checking...")
    announced = load_state()
    new_announced = 0
    try:
        streamers = api_get(f"/station/{STATION_ID}/streamers")
    except Exception as e:
        print(f"  API error: {e}")
        return

    for s in streamers:
        dname = s.get("display_name") or s["streamer_username"]
        if not s.get("broadcasts"):
            continue
        try:
            blist = api_get(f"/station/{STATION_ID}/streamer/{s['id']}/broadcasts")
        except:
            continue

        for b in reversed(blist):
            bid = b["id"]
            if bid in announced:
                break
            rec = b.get("recording")
            if not rec or not rec.get("path"):
                announced.add(bid)
                continue
            ts_s = b.get("timestampStart")
            ts_e = b.get("timestampEnd")
            secs, dur_str = dur(ts_s, ts_e)
            if secs < MIN_DURATION_MINUTES * 60:
                announced.add(bid)
                continue

            ttl = title(dname, ts_s)
            sz = round(rec.get("size", 0) / (1024 * 1024), 1)
            fw_url = find_funkwhale_url(rec["path"], dname)

            if fw_url:
                msg = (
                    f"⚫ {dname} war live auf radio zukka 🎧\n\n"
                    f"<b>{ttl}</b>\n"
                    f"⏱ {dur_str}\n\n"
                    f'📼 <a href="{fw_url}">Broadcast recording anh\u00f6ren</a>'
                )
            else:
                msg = (
                    f"⚫ {dname} war live auf radio zukka 🎧\n\n"
                    f"<b>{ttl}</b>\n"
                    f"⏱ {dur_str}"
                )

            print(f"  [{dur_str}] {ttl}")
            ok = tg_send(msg)
            print(f"    {'✓' if ok else '✗'}")
            announced.add(bid)
            if ok:
                new_announced += 1
            time.sleep(1)
            break

    save_state(announced)
    print(f"  {new_announced} new, {len(announced)} total")


if __name__ == "__main__":
    main()
