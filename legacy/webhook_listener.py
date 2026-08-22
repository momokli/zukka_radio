#!/usr/bin/env python3
"""
Zukka Radio — Post-Cast Handler
Listens for AzuraCast webhooks on DJ disconnect.
1. Posts Telegram "war live" message
2. Uploads recording to AzuraCast media + DJ playlist
3. Copies to Funkwhale and imports
4. Updates Telegram with Funkwhale link
"""

import base64
import json
import os
import shutil
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from threading import Thread

# ── Config ────────────────────────────────────────────────────────────────────
HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY", "")
STATION_ID = int(os.getenv("ZUKKA_STATION_ID", "1"))
TELEGRAM_TOKEN = os.getenv("TELEGRAM_BOT_API_KEY", "")
TELEGRAM_CHANNEL = os.getenv("TELEGRAM_CHANNEL", "@zukka_radio")
PORT = int(os.getenv("ZUKKA_WEBHOOK_PORT", "5099"))

# Funkwhale config
FW_HOST = "https://music.zukkafabrik.de"
FW_LIBRARY = "3338514d-4a10-4ae3-a017-5c90deac398c"
FW_AUTH = base64.b64encode(f"admin:{os.getenv("FUNKWHALE_PASSWORD", "")}".encode()).decode()

# DJ playlist mapping
DJ_PLAYLISTS = {"ankd": 9, "mctoastus": 10, "momo": 11}

# Paths
RECORDINGS_DIR = (
    "/var/lib/docker/volumes/azuracast_station_data/_data/zukka_radio/recordings"
)
FW_MUSIC_DIR = "/srv/funkwhale/data/music"

if not API_KEY:
    print("ERROR: ZUKKA_RADIO_API_KEY not set")
    sys.exit(1)
if not TELEGRAM_TOKEN:
    print("ERROR: TELEGRAM_BOT_API_KEY not set")
    sys.exit(1)

API_HEADERS = {"X-API-Key": API_KEY, "Accept": "application/json"}


# ── Telegram ──────────────────────────────────────────────────────────────────
def tg_send(text):
    body = json.dumps(
        {"chat_id": TELEGRAM_CHANNEL, "text": text, "disable_web_page_preview": False}
    ).encode()
    req = urllib.request.Request(
        f"https://api.telegram.org/bot{TELEGRAM_TOKEN}/sendMessage",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            resp = json.loads(r.read())
            if resp.get("ok"):
                return resp["result"]["message_id"]
    except Exception as e:
        print(f"  Telegram error: {e}")
    return None


# ── AzuraCast API ─────────────────────────────────────────────────────────────
def api_get(path):
    req = urllib.request.Request(f"{HOST}/api{path}", headers=API_HEADERS)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def api_post(path, data):
    body = json.dumps(data).encode()
    hdrs = {**API_HEADERS, "Content-Type": "application/json"}
    req = urllib.request.Request(
        f"{HOST}/api{path}", data=body, headers=hdrs, method="POST"
    )
    with urllib.request.urlopen(req, timeout=300) as r:
        return json.loads(r.read())


def api_put(path, data):
    body = json.dumps(data).encode()
    hdrs = {**API_HEADERS, "Content-Type": "application/json"}
    req = urllib.request.Request(
        f"{HOST}/api{path}", data=body, headers=hdrs, method="PUT"
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


# ── Helpers ───────────────────────────────────────────────────────────────────
def find_latest_broadcast(streamer_username):
    """Find the newest recording for a streamer."""
    streamers = api_get(f"/station/{STATION_ID}/streamers")
    for s in streamers:
        if s["streamer_username"] == streamer_username:
            blist = api_get(f"/station/{STATION_ID}/streamer/{s['id']}/broadcasts")
            for b in reversed(blist):
                rec = b.get("recording")
                if rec and rec.get("path") and rec.get("size", 0) > 20000:
                    return {
                        "id": b["id"],
                        "path": rec["path"],
                        "size": rec.get("size", 0),
                        "download_url": rec.get("downloadUrl"),
                        "timestamp_start": b.get("timestampStart"),
                        "timestamp_end": b.get("timestampEnd"),
                    }
    return None


def upload_to_azuracast(broadcast, dname):
    """Download broadcast from AzuraCast and re-upload to media library."""
    fn = Path(broadcast["path"]).name
    prefix = dname.lower()
    if not fn.lower().startswith(f"{prefix}_"):
        fn = f"{prefix}_{fn}"

    # Download
    req = urllib.request.Request(
        broadcast["download_url"], headers={"X-API-Key": API_KEY}
    )
    with urllib.request.urlopen(req, timeout=300) as r:
        file_bytes = r.read()

    # Upload via multipart
    boundary = "----FW" + os.urandom(8).hex()
    body = b""
    body += f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{fn}"\r\nContent-Type: audio/ogg\r\n\r\n'.encode()
    body += file_bytes
    body += f"\r\n--{boundary}--\r\n".encode()
    hdrs = {
        "X-API-Key": API_KEY,
        "Content-Type": f"multipart/form-data; boundary={boundary}",
    }
    req = urllib.request.Request(
        f"{HOST}/api/station/{STATION_ID}/files/upload",
        data=body,
        headers=hdrs,
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=300) as r:
        result = json.loads(r.read())

    # Find the uploaded file and assign playlist
    time.sleep(2)
    files = api_get(f"/station/{STATION_ID}/files?search={urllib.request.quote(fn)}")
    if files:
        fid = files[0]["id"]
        pl_id = DJ_PLAYLISTS.get(dname.lower(), 6)
        api_put(f"/station/{STATION_ID}/file/{fid}", {"playlists": [pl_id]})
    return fn


def copy_to_funkwhale(recording_path, dname):
    """Copy recording from AzuraCast recordings to Funkwhale music dir."""
    src = os.path.join(RECORDINGS_DIR, recording_path)
    dst_dir = os.path.join(FW_MUSIC_DIR, dname.lower())
    os.makedirs(dst_dir, exist_ok=True)
    dst = os.path.join(dst_dir, os.path.basename(recording_path))
    if not os.path.exists(dst):
        shutil.copy2(src, dst)
        os.chmod(dst, 0o644)
    # Tag with mutagen
    try:
        import re

        from mutagen.oggvorbis import OggVorbis

        audio = OggVorbis(dst)
        audio["artist"] = dname
        m = re.search(
            r"(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})", Path(recording_path).stem
        )
        if m:
            title = f"{dname} — {m.group(3)}.{m.group(2)}.{m.group(1)[2:]} {m.group(4)}:{m.group(5)}"
            audio["title"] = title
        audio.save()
    except:
        pass
    return dst


def funkwhale_import(path):
    """Run Funkwhale import for a file."""
    import subprocess

    cmd = [
        "docker",
        "compose",
        "-f",
        "/srv/funkwhale/docker-compose.yml",
        "exec",
        "-T",
        "api",
        "python",
        "/app/manage.py",
        "import_files",
        FW_LIBRARY,
        path,
        "--noinput",
        "--username",
        "admin",
    ]
    try:
        subprocess.run(cmd, capture_output=True, timeout=60)
    except:
        pass


def find_ondemand_url(recording_path, dname):
    """Find public on-demand URL on AzuraCast."""
    stem = Path(recording_path).stem
    try:
        tracks = api_get(f"/station/{STATION_ID}/ondemand")
        for t in tracks:
            text = t["media"].get("text", "")
            if stem in text or stem.replace("_", " ") in text:
                return f"{HOST}{t['download_url']}"
    except:
        pass
    return None


def find_funkwhale_url(stem, dname):
    hdrs = {"Authorization": f"Basic {FW_AUTH}"}
    try:
        q = urllib.request.quote(stem)
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/tracks/?library={FW_LIBRARY}&page_size=3&q={q}",
            headers=hdrs,
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            results = json.loads(r.read()).get("results", [])
            if results:
                return f"{FW_HOST}/library/tracks/{results[0]['id']}"
    except:
        pass
    return None


# ── Handler ───────────────────────────────────────────────────────────────────
STATE_FILE = "/opt/http/radio.zukkafabrik.de/live_state.json"

def write_live_state(streamer_name):
    """Write current live DJ state to a JSON file served by the landing page."""
    import json
    state = {"live": streamer_name is not None, "streamer": streamer_name or ""} if streamer_name else {"live": False, "streamer": ""}
    os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
    with open(STATE_FILE, "w") as f:
        json.dump(state, f)


def handle_disconnect(streamer_name):
    """Called when a DJ disconnects. Does everything needed."""
    print(f"\n{'=' * 50}")
    print(f"[{datetime.now().strftime('%H:%M:%S')}] {streamer_name} disconnected")

    # Get display name
    streamers = api_get(f"/station/{STATION_ID}/streamers")
    dname = streamer_name
    for s in streamers:
        if s["streamer_username"] == streamer_name:
            dname = s.get("display_name") or streamer_name
            break

    # 1. Wait for AzuraCast to finalize recording
    time.sleep(30)

    # 2. Find the broadcast
    bcast = find_latest_broadcast(streamer_name)
    if not bcast:
        print("  No recording found")
        return

    dur_sec = 0
    if bcast["timestamp_start"] and bcast["timestamp_end"]:
        try:
            t1 = datetime.fromisoformat(bcast["timestamp_start"].replace("Z", "+00:00"))
            t2 = datetime.fromisoformat(bcast["timestamp_end"].replace("Z", "+00:00"))
            dur_sec = int((t2 - t1).total_seconds())
        except:
            pass

    dur_min = dur_sec // 60
    ts = bcast["timestamp_start"][:10] if bcast["timestamp_start"] else "?"
    print(f"  Recording: {dur_min}min")

    # 4. Upload to AzuraCast media + playlist
    try:
        fn = upload_to_azuracast(bcast, dname)
        print(f"  AzuraCast: ✓ {fn}")
    except Exception as e:
        print(f"  AzuraCast: ✗ {e}")

    # 5. Copy to Funkwhale + import
    try:
        fw_path = copy_to_funkwhale(bcast["path"], dname)
        funkwhale_import(fw_path)
        print(f"  Funkwhale: ✓ imported")
    except Exception as e:
        print(f"  Funkwhale: ✗ {e}")

    # 5. Find Funkwhale URL
    time.sleep(3)
    stem = Path(bcast["path"]).stem
    # Prefer AzuraCast on-demand (public, no login)
    od_url = find_ondemand_url(bcast["path"], dname)
    fw_url = find_funkwhale_url(stem, dname)
    listen_url = od_url or fw_url

    # 6. Send ONE Telegram message when everything is ready
    # 6. Send ONE Telegram message when everything is ready    ts_s = bcast["timestamp_start"]    ts_e = bcast["timestamp_end"]    try:        t1 = datetime.fromisoformat(ts_s.replace("Z", "+00:00"))        t2 = datetime.fromisoformat(ts_e.replace("Z", "+00:00"))        ts_fmt = f'{t1.strftime("%d.%m.%y")} · {t1.strftime("%H:%M")} – {t2.strftime("%H:%M")}'    except:        ts_fmt = "?"        if listen_url:        clean_url = listen_url.replace("https://", "")        msg = f'⚫ {dname} war live auf radio zukka 🎧{ts_fmt}⏱ {dur_min} min📼 recording anhören{clean_url}'    else:        msg = f'⚫ {dname} war live auf radio zukka 🎧{ts_fmt}⏱ {dur_min} min'    tg_send(msg)
    print(f"  Telegram: ✓ sent")


# ── HTTP Server ───────────────────────────────────────────────────────────────
class WebhookHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        try:
            data = json.loads(body)
        except:
            self.send_response(400)
            self.end_headers()
            return

        # Check if it's a live disconnect event
        live = data.get("live", {})
        is_live = live.get("is_live", True)
        streamer = live.get("streamer_name", "")

        if not is_live and streamer:
            self.send_response(200)
            self.end_headers()
            # Write disconnected state
            write_live_state(None)
            Thread(target=handle_disconnect, args=(streamer,), daemon=True).start()
        elif is_live and streamer:
            self.send_response(200)
            self.end_headers()
            # Write connected state
            write_live_state(streamer)
        else:
            self.send_response(200)
            self.end_headers()

    def log_message(self, format, *args):
        pass  # suppress HTTP logs


if __name__ == "__main__":
    print(f"Post-Cast Handler listening on port {PORT}")
    HTTPServer(("127.0.0.1", PORT), WebhookHandler).serve_forever()
