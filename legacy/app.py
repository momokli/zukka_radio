"""
Zukka Radio — Broadcast Uploader + DJ Library
Flask web app to review, import, and manage streamer broadcasts.
"""

import base64
import json
import os
import re
import time
from datetime import datetime, timezone
from pathlib import Path
from queue import Queue
from threading import Thread

import requests
from dotenv import load_dotenv
from flask import Flask, Response, jsonify, render_template, request

load_dotenv()

app = Flask(__name__)

# ── Config ────────────────────────────────────────────────────────────────────
HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY")
STATION_ID = 1
PLAYLIST_ID = 6  # id of the "broadcasts" playlist (rotation pool)
STATE_FILE = Path(__file__).parent / "uploaded_state.json"
TOGGLE_FILE = Path(__file__).parent / "broadcast_toggles.json"

# Broadcast playlist IDs (DJ-fresh + rotation pool)
BROADCAST_PLAYLIST_IDS = {6, 9, 10, 11}

# DJ playlist mapping (display_name → playlist ID)
DJ_PLAYLIST_MAP = {"ankd": 9, "mctoastus": 10, "momo": 11}

if not API_KEY:
    raise RuntimeError("ZUKKA_RADIO_API_KEY not set in .env")

API_HEADERS = {
    "X-API-Key": API_KEY,
    "Content-Type": "application/json",
    "Accept": "application/json",
}


# ── State helpers ─────────────────────────────────────────────────────────────
def load_state() -> dict:
    if STATE_FILE.exists():
        return json.loads(STATE_FILE.read_text())
    return {"uploaded": {}, "last_fetched": None}


def save_state(state: dict) -> None:
    STATE_FILE.write_text(json.dumps(state, indent=2))


def mark_uploaded(broadcast_id: int, streamer_name: str, title: str, path: str) -> None:
    state = load_state()
    state["uploaded"][str(broadcast_id)] = {
        "streamer": streamer_name,
        "title": title,
        "path": path,
        "uploaded_at": datetime.now(timezone.utc).isoformat(),
    }
    state["last_fetched"] = datetime.now(timezone.utc).isoformat()
    save_state(state)


def is_uploaded(broadcast_id: int) -> bool:
    return str(broadcast_id) in load_state().get("uploaded", {})


# ── Toggle state (DJ library on/off switches) ─────────────────────────────────
def load_toggles() -> dict:
    """Load broadcast toggles: {file_id: {in_rotation: bool, ...}}"""
    if TOGGLE_FILE.exists():
        return json.loads(TOGGLE_FILE.read_text())
    return {}


def save_toggles(toggles: dict) -> None:
    TOGGLE_FILE.write_text(json.dumps(toggles, indent=2))


def get_toggle(file_id: int) -> bool:
    """Check if a file is toggled ON. Default: True (in rotation)."""
    toggles = load_toggles()
    return toggles.get(str(file_id), {}).get("in_rotation", True)


def set_toggle(file_id: int, in_rotation: bool, toggled_by: str = "unknown") -> None:
    """Set a file's toggle state and persist."""
    toggles = load_toggles()
    toggles[str(file_id)] = {
        "in_rotation": in_rotation,
        "toggled_by": toggled_by,
        "toggled_at": datetime.now(timezone.utc).isoformat(),
    }
    save_toggles(toggles)


# ── AzuraCast API helpers ─────────────────────────────────────────────────────
def api_get(path: str) -> dict | list:
    url = f"{HOST}/api{path}"
    r = requests.get(url, headers=API_HEADERS, timeout=30)
    r.raise_for_status()
    return r.json()


def api_post(path: str, data: dict) -> dict:
    url = f"{HOST}/api{path}"
    r = requests.post(url, headers=API_HEADERS, json=data, timeout=60)
    r.raise_for_status()
    return r.json()


def api_put(path: str, data: dict) -> dict:
    url = f"{HOST}/api{path}"
    r = requests.put(url, headers=API_HEADERS, json=data, timeout=30)
    r.raise_for_status()
    return r.json()


def api_download(url: str) -> bytes:
    r = requests.get(url, headers={"X-API-Key": API_KEY}, timeout=120)
    r.raise_for_status()
    return r.content


def api_stream(url: str):
    """Stream a file from AzuraCast (returns response iterator)."""
    return requests.get(url, headers={"X-API-Key": API_KEY}, stream=True, timeout=300)


# ── Naming & metadata helpers ─────────────────────────────────────────────────
def compute_target_filename(display_name: str, recording_path: str) -> str:
    base = Path(recording_path).name
    prefix = display_name.lower()
    if not base.lower().startswith(f"{prefix}_"):
        base = f"{prefix}_{base}"
    return base


def broadcast_display_title(
    display_name: str, timestamp_start: str | None, recording_path: str
) -> str:
    if timestamp_start:
        try:
            dt = datetime.fromisoformat(timestamp_start.replace("Z", "+00:00"))
            return f"{display_name} - {dt.strftime('%d.%m.%y %H:%M')}"
        except (ValueError, AttributeError):
            pass
    name = Path(recording_path).stem
    m = re.search(r"(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})", name)
    if m:
        return f"{display_name} - {m[3]}.{m[2]}.{m[1][2:]} {m[4]}:{m[5]}"
    return f"{display_name} - {name}"


def compute_duration(
    timestamp_start: str | None, timestamp_end: str | None
) -> tuple[int, str]:
    if timestamp_start and timestamp_end:
        try:
            t1 = datetime.fromisoformat(timestamp_start.replace("Z", "+00:00"))
            t2 = datetime.fromisoformat(timestamp_end.replace("Z", "+00:00"))
            secs = int((t2 - t1).total_seconds())
            if secs < 0:
                secs = 0
            h, m = divmod(secs, 3600)
            m, s = divmod(m, 60)
            if h > 0:
                text = f"{h}:{m:02d}:{s:02d}"
            else:
                text = f"{m}:{s:02d}"
            return secs, text
        except (ValueError, AttributeError):
            pass
    return 0, "?"


# ── DJ tag extraction from filename ───────────────────────────────────────────
def dj_from_filename(path: str) -> str | None:
    """Extract DJ display name from a media filename prefix."""
    for dj_name in DJ_PLAYLIST_MAP:
        if path.lower().startswith(dj_name):
            return dj_name
    return None


# ── Fetch broadcasts (for uploader view) ──────────────────────────────────────
def fetch_existing_media_paths() -> set[str]:
    paths = set()
    page = 1
    while True:
        try:
            files = api_get(f"/station/{STATION_ID}/files?page={page}&perPage=200")
        except requests.HTTPError:
            break
        if not files:
            break
        for f in files:
            paths.add(f["path"])
        if len(files) < 200:
            break
        page += 1
    return paths


def fetch_all_broadcasts() -> list[dict]:
    streamers = api_get(f"/station/{STATION_ID}/streamers")
    existing_paths = fetch_existing_media_paths()
    results = []
    auto_marked = False

    for s in streamers:
        streamer_id = s["id"]
        streamer_username = s["streamer_username"]
        display_name = s.get("display_name") or streamer_username

        if not s.get("broadcasts"):
            continue

        try:
            broadcasts_list = api_get(
                f"/station/{STATION_ID}/streamer/{streamer_id}/broadcasts"
            )
        except requests.HTTPError:
            continue

        for b in broadcasts_list:
            recording = b.get("recording")
            if not recording or not recording.get("path"):
                continue

            broadcast_id = b["id"]
            recording_path = recording["path"]
            target_filename = compute_target_filename(display_name, recording_path)

            already_uploaded = is_uploaded(broadcast_id)
            already_in_media = target_filename in existing_paths

            if not already_uploaded and already_in_media:
                mark_uploaded(
                    broadcast_id, display_name, recording_path, target_filename
                )
                already_uploaded = True
                auto_marked = True

            ts_start = b.get("timestampStart")
            ts_end = b.get("timestampEnd")
            display_title = broadcast_display_title(
                display_name, ts_start, recording_path
            )
            duration_secs, duration_text = compute_duration(ts_start, ts_end)

            results.append(
                {
                    "id": broadcast_id,
                    "streamer_username": streamer_username,
                    "display_name": display_name,
                    "recording_path": recording_path,
                    "target_filename": target_filename,
                    "display_title": display_title,
                    "duration_seconds": duration_secs,
                    "duration_text": duration_text,
                    "size_bytes": recording.get("size", 0),
                    "size_mb": round(recording.get("size", 0) / (1024 * 1024), 2),
                    "download_url": recording["downloadUrl"],
                    "timestamp_start": ts_start,
                    "timestamp_end": ts_end,
                    "uploaded": already_uploaded,
                }
            )

    if auto_marked:
        save_state(load_state())

    def sort_key(b):
        ts = b.get("timestamp_start") or ""
        return (b["uploaded"], ts)

    results.sort(key=sort_key)
    return results


# ── Upload logic ──────────────────────────────────────────────────────────────
def upload_single_broadcast(broadcast: dict, progress_queue: Queue) -> bool:
    bid = broadcast["id"]
    display_name = broadcast["display_name"]
    title = broadcast.get("display_title") or broadcast.get("recording_path", "Unknown")
    download_url = broadcast["download_url"]
    filename = compute_target_filename(
        broadcast["display_name"], broadcast["recording_path"]
    )

    progress_queue.put(
        {"id": bid, "status": "downloading", "message": f"Downloading {filename}..."}
    )

    try:
        file_bytes = api_download(download_url)
        file_b64 = base64.b64encode(file_bytes).decode("utf-8")

        progress_queue.put(
            {
                "id": bid,
                "status": "uploading",
                "message": f"Uploading {filename} ({broadcast['size_mb']} MB)...",
            }
        )

        upload_result = api_post(
            f"/station/{STATION_ID}/files",
            {"path": filename, "file": file_b64},
        )
        file_id = upload_result["id"]

        api_put(
            f"/station/{STATION_ID}/file/{file_id}",
            {"title": title, "playlists": [PLAYLIST_ID]},
        )

        mark_uploaded(bid, display_name, title, filename)

        progress_queue.put(
            {"id": bid, "status": "done", "message": f"Done — added to broadcasts"}
        )
        return True

    except Exception as e:
        progress_queue.put({"id": bid, "status": "error", "message": str(e)})
        return False


# ── Library: fetch files from media that are in broadcast playlists ───────────
def fetch_library_files(dj_filter: str | None = None) -> list[dict]:
    """Fetch all media files in broadcast playlists, with toggle state."""
    all_files = []
    page = 1
    while True:
        try:
            files = api_get(f"/station/{STATION_ID}/files?page={page}&perPage=200")
        except requests.HTTPError:
            break
        if not files:
            break
        all_files.extend(files)
        if len(files) < 200:
            break
        page += 1

    results = []
    for f in all_files:
        pl_ids = {p["id"] for p in f.get("playlists", [])}
        if not (pl_ids & BROADCAST_PLAYLIST_IDS):
            continue

        path = f["path"]
        dj = dj_from_filename(path)
        if not dj:
            continue

        if dj_filter and dj.lower() != dj_filter.lower():
            continue

        file_id = f["id"]
        in_rotation = get_toggle(file_id)
        current_playlists = [p["name"] for p in f.get("playlists", [])]

        results.append(
            {
                "id": file_id,
                "path": path,
                "title": f.get("title", path),
                "artist": f.get("artist", ""),
                "length_text": f.get("length_text", "?"),
                "length_seconds": f.get("length", 0),
                "size_mb": 0,  # not in file listing; could fetch individually
                "dj": dj,
                "in_rotation": in_rotation,
                "playlists": current_playlists,
                "mtime": f.get("mtime", 0),
                "art": f.get("art", ""),
                "links": f.get("links", {}),
            }
        )

    results.sort(key=lambda f: f.get("mtime", 0), reverse=True)
    return results


# ── Sync: apply toggles to AzuraCast ──────────────────────────────────────────
def sync_toggle_to_azuracast(file_id: int, in_rotation: bool) -> bool:
    """Add or remove a file from its DJ playlist based on toggle state."""
    try:
        file_data = api_get(f"/station/{STATION_ID}/file/{file_id}")
    except requests.HTTPError:
        return False

    path = file_data["path"]
    dj = dj_from_filename(path)
    if not dj:
        return False

    dj_playlist_id = DJ_PLAYLIST_MAP[dj]
    current_pls = [p["id"] for p in file_data.get("playlists", [])]

    if in_rotation:
        # Add to DJ playlist if not already there
        if dj_playlist_id not in current_pls:
            current_pls.append(dj_playlist_id)
    else:
        # Remove from DJ playlist
        current_pls = [p for p in current_pls if p != dj_playlist_id]

    api_put(
        f"/station/{STATION_ID}/file/{file_id}",
        {"playlists": current_pls},
    )
    return True


def sync_all_toggles() -> dict:
    """Sync all toggled files. Returns {synced: N, errors: N}."""
    toggles = load_toggles()
    synced = 0
    errors = 0

    for file_id_str, state in toggles.items():
        try:
            if sync_toggle_to_azuracast(int(file_id_str), state["in_rotation"]):
                synced += 1
            else:
                errors += 1
        except Exception:
            errors += 1

    return {"synced": synced, "errors": errors}


def background_sync_loop():
    """Run periodic sync in a background thread."""
    while True:
        time.sleep(60)
        try:
            sync_all_toggles()
        except Exception:
            pass


# ── Routes: Uploader ──────────────────────────────────────────────────────────
@app.route("/")
def index():
    return render_template("index.html")


@app.route("/api/broadcasts")
def api_broadcasts():
    try:
        broadcasts = fetch_all_broadcasts()
        state = load_state()
        return jsonify(
            {
                "broadcasts": broadcasts,
                "uploaded_count": len(state.get("uploaded", {})),
                "last_fetched": state.get("last_fetched"),
            }
        )
    except requests.HTTPError as e:
        return jsonify({"error": f"AzuraCast API error: {e}"}), 502
    except Exception as e:
        return jsonify({"error": str(e)}), 500


@app.route("/api/upload", methods=["POST"])
def api_upload():
    data = request.get_json()
    if not data or "ids" not in data:
        return jsonify({"error": "Missing 'ids' in request body"}), 400

    selected_ids = set(data["ids"])
    all_broadcasts = fetch_all_broadcasts()
    to_upload = [
        b for b in all_broadcasts if b["id"] in selected_ids and not b["uploaded"]
    ]

    if not to_upload:
        return jsonify({"error": "No valid broadcasts selected"}), 400

    progress_queue: Queue = Queue()

    def generate():
        def worker():
            for b in to_upload:
                upload_single_broadcast(b, progress_queue)

        Thread(target=worker, daemon=True).start()

        done_count = 0
        total = len(to_upload)
        while done_count < total:
            event = progress_queue.get()
            yield f"data: {json.dumps(event)}\n\n"
            if event["status"] in ("done", "error"):
                done_count += 1

        yield f"data: {json.dumps({'status': 'complete', 'total': total})}\n\n"

    return Response(
        generate(),
        mimetype="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "X-Accel-Buffering": "no",
        },
    )


@app.route("/api/state")
def api_state():
    return jsonify(load_state())


# ── Routes: DJ Library ───────────────────────────────────────────────────────
@app.route("/library")
def library():
    """Serve the DJ library page."""
    dj = request.args.get("dj", "")
    return render_template("library.html", dj_filter=dj)


@app.route("/api/library")
def api_library():
    """Return all broadcast files with toggle state, optionally filtered by DJ."""
    dj = request.args.get("dj", "")
    dj_filter = dj if dj else None
    try:
        files = fetch_library_files(dj_filter)
        return jsonify({"files": files, "djs": sorted(DJ_PLAYLIST_MAP.keys())})
    except Exception as e:
        return jsonify({"error": str(e)}), 500


@app.route("/api/library/toggle", methods=["POST"])
def api_library_toggle():
    """Toggle a file in/out of rotation and apply immediately."""
    data = request.get_json()
    if not data or "file_id" not in data or "in_rotation" not in data:
        return jsonify({"error": "Missing file_id or in_rotation"}), 400

    file_id = data["file_id"]
    in_rotation = data["in_rotation"]
    toggled_by = data.get("toggled_by", "web-ui")

    # Save toggle state
    set_toggle(file_id, in_rotation, toggled_by)

    # Apply immediately
    success = sync_toggle_to_azuracast(file_id, in_rotation)

    return jsonify(
        {
            "success": success,
            "file_id": file_id,
            "in_rotation": in_rotation,
        }
    )


@app.route("/api/library/stream/<int:file_id>")
def api_library_stream(file_id: int):
    """Stream a file directly from AzuraCast."""
    try:
        file_data = api_get(f"/station/{STATION_ID}/file/{file_id}")
    except requests.HTTPError:
        return jsonify({"error": "File not found"}), 404

    play_url = file_data.get("links", {}).get("play", "")
    if not play_url:
        return jsonify({"error": "No stream URL"}), 404

    az_resp = api_stream(play_url)
    return Response(
        az_resp.iter_content(chunk_size=8192),
        status=az_resp.status_code,
        headers={
            "Content-Type": az_resp.headers.get("Content-Type", "audio/ogg"),
            "Content-Disposition": az_resp.headers.get("Content-Disposition", "inline"),
        },
    )


@app.route("/api/library/download/<int:file_id>")
def api_library_download(file_id: int):
    """Download a file from AzuraCast."""
    try:
        file_data = api_get(f"/station/{STATION_ID}/file/{file_id}")
    except requests.HTTPError:
        return jsonify({"error": "File not found"}), 404

    play_url = file_data.get("links", {}).get("play", "")
    if not play_url:
        return jsonify({"error": "No download URL"}), 404

    file_bytes = api_download(play_url)
    filename = file_data.get("path", "download.ogg")

    return Response(
        file_bytes,
        headers={
            "Content-Type": "audio/ogg",
            "Content-Disposition": f'attachment; filename="{filename}"',
        },
    )


@app.route("/api/library/sync", methods=["POST"])
def api_library_sync():
    """Manually trigger a full toggle sync."""
    result = sync_all_toggles()
    return jsonify(result)


# ── Main ──────────────────────────────────────────────────────────────────────
if __name__ == "__main__":
    # Start background sync thread
    Thread(target=background_sync_loop, daemon=True).start()
    app.run(host="127.0.0.1", port=5000, debug=True)
