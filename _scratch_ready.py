#!/usr/bin/env python3
"""Scratch: final readiness check (API side)."""

import json
import urllib.request

env = {}
for line in open(".env"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        env[k.strip()] = v.strip().strip('"').strip("'")

HOST = env.get("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
KEY = env.get("ZUKKA_RADIO_API_KEY", "")
HDRS = {"X-API-Key": KEY, "Accept": "application/json"}


def get(path):
    req = urllib.request.Request(HOST + "/api" + path, headers=HDRS)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read() or b"null")


pl = get("/station/1/playlist/15")
print("PLAYLIST 15")
print(f"  name={pl['name']!r} enabled={pl['is_enabled']} songs={pl['num_songs']}")
print(f"  backend_options={pl['backend_options']}")
print(f"  requests={pl['include_in_requests']} on_demand={pl['include_in_on_demand']}")
print(f"  schedule={json.dumps(pl['schedule_items'])}")

f = get("/station/1/file/108")
print("\nFILE 108")
print(f"  title={f.get('title')!r}")
print(f"  artist={f.get('artist')!r} album={f.get('album')!r}")
print(
    f"  length={f.get('length')}s playlists={[p['id'] for p in f.get('playlists') or []]}"
)

np = get("/nowplaying/1")
print("\nSTATION")
print(f"  online={np.get('is_online')} live={np.get('live', {}).get('is_live')}")
print(f"  now_playing={np.get('now_playing', {}).get('song', {}).get('text')!r}")
print(f"  playlist={np.get('now_playing', {}).get('playlist')!r}")

pls = get("/station/1/playlists")
print("\nALL PLAYLISTS")
for p in pls:
    flag = " <-- scheduled" if p.get("schedule_items") else ""
    print(
        f"  id={p['id']:2} w={p['weight']:3} songs={p['num_songs']:3} "
        f"enabled={p['is_enabled']} {p['name']!r}{flag}"
    )
