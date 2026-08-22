#!/usr/bin/env python3
"""
radio zukka — Live/DJ Notifier
===============================
Handles AzuraCast live_connect and live_disconnect webhooks.
Sends instant Telegram messages only. No upload, no wait.

ON:  🔴 duck ist jetzt live auf radio zukka 🎧
OFF: ⚫ duck war live auf radio zukka 🎧
"""

import json
import os
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer
from threading import Thread

# ── Config ────────────────────────────────────────────────────────────
HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY", "")
STATION_ID = int(os.getenv("ZUKKA_STATION_ID", "1"))
TELEGRAM_TOKEN = os.getenv("TELEGRAM_BOT_API_KEY", "")
TELEGRAM_CHANNEL = os.getenv("TELEGRAM_CHANNEL", "@zukka_radio")
PORT = int(os.getenv("ZUKKA_WEBHOOK_PORT", "5099"))

DISPLAY_NAMES = {"hovio": "ANKD", "mctoastus": "duck", "momo": "momo"}
STATE_FILE = "/opt/http/radio.zukkafabrik.de/live_state.json"

if not API_KEY:
    sys.exit("ERROR: ZUKKA_RADIO_API_KEY not set")
if not TELEGRAM_TOKEN:
    sys.exit("ERROR: TELEGRAM_BOT_API_KEY not set")

API_HEADERS = {"X-API-Key": API_KEY, "Accept": "application/json"}

print(f"radio zukka notifier — port {PORT}")
print(f"  telegram → {TELEGRAM_CHANNEL}")


# ── Helpers ───────────────────────────────────────────────────────────


def api_get(path):
    req = urllib.request.Request(f"{HOST}/api{path}", headers=API_HEADERS)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def tg_send(text):
    body = json.dumps(
        {
            "chat_id": TELEGRAM_CHANNEL,
            "text": text,
            "disable_web_page_preview": False,
        }
    ).encode()
    req = urllib.request.Request(
        f"https://api.telegram.org/bot{TELEGRAM_TOKEN}/sendMessage",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read()).get("ok", False)
    except Exception as e:
        print(f"  ✗ Telegram: {e}")
        return False


def get_display_name(username):
    if username in DISPLAY_NAMES:
        return DISPLAY_NAMES[username]
    try:
        for s in api_get(f"/station/{STATION_ID}/streamers"):
            if s["streamer_username"] == username:
                return s.get("display_name") or username
    except Exception:
        pass
    return username


def write_live_state(streamer_name):
    state = {"live": streamer_name is not None, "streamer": streamer_name or ""}
    os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
    with open(STATE_FILE, "w") as f:
        json.dump(state, f)


def load_live_state():
    try:
        with open(STATE_FILE) as f:
            return json.load(f)
    except Exception:
        return {"live": False, "streamer": ""}


# ── Handlers ──────────────────────────────────────────────────────────


def handle_connect(streamer_name):
    try:
        dname = get_display_name(streamer_name)
        write_live_state(dname)
        msg = (
            f"🔴 {dname} ist jetzt live auf radio zukka 🎧\n\n"
            f"🌐 web player\nradio.zukkafabrik.de"
        )
        ok = tg_send(msg)
        print(
            f"[{datetime.now().strftime('%H:%M:%S')}] LIVE  {dname}  {'✓' if ok else '✗'}"
        )
    except Exception as e:
        print(f"LIVE ERROR: {e}")


def handle_disconnect(streamer_name):
    try:
        dname = get_display_name(streamer_name)
        write_live_state(None)
        msg = f"⚫ {dname} war live auf radio zukka 🎧"
        ok = tg_send(msg)
        print(
            f"[{datetime.now().strftime('%H:%M:%S')}] OFF   {dname}  {'✓' if ok else '✗'}"
        )
    except Exception as e:
        print(f"OFF ERROR: {e}")


# ── Server ────────────────────────────────────────────────────────────


class WebhookHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        try:
            data = json.loads(self.rfile.read(length))
        except Exception:
            self.send_response(400)
            self.end_headers()
            return

        live = data.get("live", {})
        is_live = live.get("is_live", True)
        streamer = live.get("streamer_name", "")

        if not is_live and not streamer:
            prev = load_live_state()
            if prev.get("live") and prev.get("streamer"):
                streamer = prev["streamer"]
            else:
                self.send_response(200)
                self.end_headers()
                return

        self.send_response(200)
        self.end_headers()

        if streamer:
            fn = handle_connect if is_live else handle_disconnect
            Thread(target=fn, args=(streamer,), daemon=True).start()

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", PORT), WebhookHandler).serve_forever()
