#!/usr/bin/env python3
"""Move broadcasts older than 30 days from DJ-fresh playlists to the shared rotation pool."""
import json, os, sys, urllib.request, urllib.error
from datetime import datetime, timezone, timedelta

HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY", "")
STATION_ID = int(os.getenv("ZUKKA_STATION_ID", "1"))
MAX_AGE_DAYS = int(os.getenv("ZUKKA_MAX_AGE_DAYS", "30"))

if not API_KEY:
    print("ERROR: ZUKKA_RADIO_API_KEY not set", file=sys.stderr); sys.exit(1)

HEADERS = {"X-API-Key": API_KEY, "Accept": "application/json", "Content-Type": "application/json"}

# DJ-fresh playlist IDs -> target shared pool
DJ_FRESH = {9, 10, 11}  # broadcasts-ankd, broadcasts-mctoastus, broadcasts-momo
SHARED_POOL = 6  # broadcasts

def api_get(path):
    req = urllib.request.Request(f"{HOST}/api{path}", headers=HEADERS)
    with urllib.request.urlopen(req, timeout=30) as r: return json.loads(r.read())

def api_put(path, data):
    body = json.dumps(data).encode()
    req = urllib.request.Request(f"{HOST}/api{path}", data=body, headers=HEADERS, method="PUT")
    with urllib.request.urlopen(req, timeout=30) as r: return json.loads(r.read())

cutoff = datetime.now(timezone.utc) - timedelta(days=MAX_AGE_DAYS)
moved = 0

# Get all files
page = 1
while True:
    try: files = api_get(f"/station/{STATION_ID}/files?page={page}&perPage=200")
    except: break
    if not files: break
    
    for f in files:
        pl_ids = {p["id"] for p in f.get("playlists", [])}
        fresh_pls = pl_ids & DJ_FRESH
        if not fresh_pls:
            continue
        
        # Check upload age
        uploaded = f.get("uploaded_at") or f.get("mtime")
        if not uploaded:
            continue
        
        try:
            age = datetime.fromtimestamp(uploaded, tz=timezone.utc)
        except:
            continue
        
        if age > cutoff:
            continue  # still fresh
        
        # Move: remove from all DJ-fresh, add to shared pool
        new_pls = list(pl_ids - DJ_FRESH) + [SHARED_POOL]
        api_put(f"/station/{STATION_ID}/file/{f['id']}", {"playlists": new_pls})
        print(f"  aged out: {f['path']} ({age.strftime('%d.%m.%y')})")
        moved += 1
    
    if len(files) < 200: break
    page += 1

print(f"Moved {moved} track(s) to shared rotation pool.")
