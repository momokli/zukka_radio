#!/usr/bin/env python3
"""Zukka Radio Auto Uploader v2 — multipart upload, no base64 overhead."""

import argparse, base64, json, os, sys, urllib.error, urllib.request, urllib.parse
from datetime import datetime, timezone
from pathlib import Path

HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY", "")
STATION_ID = int(os.getenv("ZUKKA_STATION_ID", "1"))
PLAYLIST_ID = int(os.getenv("ZUKKA_PLAYLIST_ID", "6"))
DJ_PLAYLISTS = {"ankd": 9, "mctoastus": 10, "momo": 11}
STATE_FILE = Path(os.getenv("ZUKKA_STATE_FILE", "auto_upload_state.json"))
READ_FROM_DISK = os.getenv("ZUKKA_READ_FROM_DISK", "") == "1"
RECORDINGS_DIR = os.getenv("ZUKKA_RECORDINGS_DIR", "/var/azuracast/stations/zukka_radio/recordings")

if not API_KEY:
    print("ERROR: ZUKKA_RADIO_API_KEY not set", file=sys.stderr); sys.exit(1)

API_HEADERS = {"X-API-Key": API_KEY, "Accept": "application/json"}

def api_get(path):
    req = urllib.request.Request(f"{HOST}/api{path}", headers=API_HEADERS)
    with urllib.request.urlopen(req, timeout=30) as r: return json.loads(r.read())

def api_put(path, data):
    body = json.dumps(data).encode()
    headers = {**API_HEADERS, "Content-Type": "application/json"}
    req = urllib.request.Request(f"{HOST}/api{path}", data=body, headers=headers, method="PUT")
    with urllib.request.urlopen(req, timeout=30) as r: return json.loads(r.read())

def multipart_upload(file_bytes, filename):
    boundary = "----FormBoundary" + os.urandom(16).hex()
    body = b""
    body += f"--{boundary}\r\n".encode()
    body += f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'.encode()
    body += b"Content-Type: application/octet-stream\r\n\r\n"
    body += file_bytes
    body += f"\r\n--{boundary}--\r\n".encode()
    headers = {"X-API-Key": API_KEY, "Content-Type": f"multipart/form-data; boundary={boundary}"}
    req = urllib.request.Request(f"{HOST}/api/station/{STATION_ID}/files/upload", data=body, headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=300) as r: return json.loads(r.read())

# -- state --
def load_state():
    if STATE_FILE.exists(): return json.loads(STATE_FILE.read_text())
    return {"uploaded": {}}

def save_state(s): STATE_FILE.write_text(json.dumps(s, indent=2))

def is_uploaded(bid):
    return str(bid) in load_state().get("uploaded", {})

def mark_uploaded(bid, info):
    s = load_state()
    s["uploaded"][str(bid)] = {**info, "uploaded_at": datetime.now(timezone.utc).isoformat()}
    save_state(s)

# -- naming --
def target_filename(dname, rpath):
    base = Path(rpath).name
    prefix = dname.lower()
    if not base.lower().startswith(f"{prefix}_"): base = f"{prefix}_{base}"
    return base

def display_title(dname, ts):
    if ts:
        try:
            dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
            return f"{dname} - {dt.strftime('%d.%m.%y %H:%M')}"
        except: pass
    return f"{dname} - unknown"

def duration_mins(ts_start, ts_end):
    if not ts_start or not ts_end: return 0.0
    try:
        t1 = datetime.fromisoformat(ts_start.replace("Z", "+00:00"))
        t2 = datetime.fromisoformat(ts_end.replace("Z", "+00:00"))
        return max(0, (t2 - t1).total_seconds()) / 60.0
    except: return 0.0

# -- fetch --
def existing_media_paths():
    paths = set(); page = 1
    while True:
        try: files = api_get(f"/station/{STATION_ID}/files?page={page}&perPage=200")
        except: break
        if not files: break
        for f in files: paths.add(f["path"])
        if len(files) < 200: break
        page += 1
    return paths

def fetch_broadcasts():
    streamers = api_get(f"/station/{STATION_ID}/streamers")
    existing = existing_media_paths()
    results = []
    for s in streamers:
        if not s.get("broadcasts"): continue
        try: blist = api_get(f"/station/{STATION_ID}/streamer/{s['id']}/broadcasts")
        except: continue
        dname = s.get("display_name") or s["streamer_username"]
        for b in blist:
            rec = b.get("recording")
            if not rec or not rec.get("path"): continue
            bid = b["id"]; ts_s = b.get("timestampStart"); ts_e = b.get("timestampEnd")
            mins = duration_mins(ts_s, ts_e); fname = target_filename(dname, rec["path"])
            up = is_uploaded(bid) or fname in existing
            if fname in existing and not is_uploaded(bid):
                mark_uploaded(bid, {"streamer": dname, "title": display_title(dname, ts_s), "path": fname, "duration_minutes": mins})
            results.append({"id": bid, "display_name": dname, "recording_path": rec["path"],
                "size_mb": round(rec.get("size",0)/(1024*1024),2), "download_url": rec["downloadUrl"],
                "timestamp_start": ts_s, "timestamp_end": ts_e, "duration_minutes": round(mins,1), "uploaded": up})
    results.sort(key=lambda b: b.get("timestamp_start") or "")
    return results

# -- upload --
def upload_one(b, dry_run=False):
    bid = b["id"]; fname = target_filename(b["display_name"], b["recording_path"])
    title = display_title(b["display_name"], b["timestamp_start"])
    print(f"  [{b['duration_minutes']:.0f} min, {b['size_mb']:.1f} MB] {title} -> {fname}", end=" ")
    if dry_run: print("(dry)"); return False
    try:
        if READ_FROM_DISK:
            file_bytes = Path(os.path.join(RECORDINGS_DIR, b["recording_path"])).read_bytes()
        else:
            req = urllib.request.Request(b["download_url"], headers={"X-API-Key": API_KEY})
            with urllib.request.urlopen(req, timeout=300) as r: file_bytes = r.read()
        result = multipart_upload(file_bytes, fname)
        fid = result.get("id")
        if not fid:
            found = api_get(f"/station/{STATION_ID}/files?search={urllib.parse.quote(fname)}")
            if found and len(found) > 0: fid = found[0]["id"]
            else: raise Exception(f"uploaded but cannot find '{fname}'")
        api_put(f"/station/{STATION_ID}/file/{fid}", {"title": title, "playlists": [DJ_PLAYLISTS.get(b["display_name"].lower(), PLAYLIST_ID)]})
        mark_uploaded(bid, {"streamer": b["display_name"], "title": title, "path": fname, "duration_minutes": b["duration_minutes"]})
        print("ok"); return True
    except Exception as e: print(f"FAIL {e}"); return False

# -- main --
def main():
    p = argparse.ArgumentParser()
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--min-duration", type=int, default=15)
    p.add_argument("--max-duration", type=int, default=0)
    p.add_argument("--streamer", type=str, default="")
    p.add_argument("--since", type=str, default="")
    args = p.parse_args()
    since_date = None
    if args.since: since_date = datetime.strptime(args.since, "%Y-%m-%d").replace(tzinfo=timezone.utc)

    print("Fetching...")
    all_bs = fetch_broadcasts()
    candidates = [b for b in all_bs if not b["uploaded"]
        and b["duration_minutes"] >= args.min_duration
        and (args.max_duration == 0 or b["duration_minutes"] <= args.max_duration)
        and (not args.streamer or b["display_name"].lower() == args.streamer.lower())
        and (not since_date or (b["timestamp_start"] and b["timestamp_start"] >= args.since))]
    skipped = sum(1 for b in all_bs if not b["uploaded"] and b["duration_minutes"] < args.min_duration)
    already = sum(1 for b in all_bs if b["uploaded"])
    print(f"Found {len(all_bs)} total: {already} uploaded, {len(candidates)} new >= {args.min_duration} min" + (f", {skipped} short" if skipped else ""))
    if not candidates: print("Nothing to upload."); return
    print()
    if args.dry_run:
        print(f"Would upload {len(candidates)}:")
        for b in candidates:
            print(f"  [{b['duration_minutes']:.0f} min, {b['size_mb']:.1f} MB] {display_title(b['display_name'], b['timestamp_start'])} -> {target_filename(b['display_name'], b['recording_path'])}")
        return
    print(f"Uploading {len(candidates)}...")
    ok = fail = 0
    for b in candidates:
        if upload_one(b): ok += 1
        else: fail += 1
    print(f"\nDone. {ok} ok, {fail} failed.")

if __name__ == "__main__": main()
