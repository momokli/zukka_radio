#!/usr/bin/env python3
"""
radio zukka — Recording Watcher
================================
Runs every 2 minutes (systemd timer). Finds new finalized recordings,
uploads to AzuraCast + Funkwhale, posts Telegram link.

ON/OFF messages are handled by pipeline.py (webhook).
This only handles 📼 "recording anhören" posts.

Robustness (issue #2/#3):
  - No secrets in code: all credentials come from the environment
    (.env / systemd EnvironmentFile).
  - Paths/IDs are env-configurable with sane defaults.
  - stdout is line-buffered so log lines reach journald immediately.
"""

import base64
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

# Issue #4: optional recording post-processing (ffmpeg master chain).
# Guarded import: if the module is missing/broken the watcher still runs
# and simply skips post-processing.
try:
    import postprocess
except Exception as _e:
    print(
        f"WARNING: postprocess module unavailable — post-processing disabled ({_e})",
        file=sys.stderr,
    )
    postprocess = None

# systemd/journald captures stdout — make sure every line is flushed
# immediately (Python block-buffers piped stdout otherwise).
try:
    sys.stdout.reconfigure(line_buffering=True)
except Exception:
    pass

# ── Config ────────────────────────────────────────────────────────────
HOST = os.getenv("ZUKKA_RADIO_HOST", "https://cast.zukkafabrik.de").rstrip("/")
API_KEY = os.getenv("ZUKKA_RADIO_API_KEY", "")
STATION_ID = int(os.getenv("ZUKKA_STATION_ID", "1"))
TELEGRAM_TOKEN = os.getenv("TELEGRAM_BOT_API_KEY", "")
TELEGRAM_CHANNEL = os.getenv("TELEGRAM_CHANNEL", "@zukka_radio")

FW_HOST = "https://music.zukkafabrik.de"
FW_LIBRARY = os.getenv("FUNKWHALE_LIBRARY", "3338514d-4a10-4ae3-a017-5c90deac398c")
FW_COMPOSE = os.getenv("FUNKWHALE_COMPOSE", "/srv/funkwhale/docker-compose.yml")

# Funkwhale admin auth — from env, no hardcoded secrets
FW_ADMIN = os.getenv("FUNKWHALE_ADMIN", "admin")
FW_PASSWORD = os.getenv("FUNKWHALE_PASSWORD", "")


def build_fw_auth(admin, password):
    """Return the Basic auth header for the Funkwhale API.

    Empty password → empty header (Funkwhale steps are skipped
    gracefully). Kept as a pure function so tests can verify it.
    """
    if not password:
        return ""
    return "Basic " + base64.b64encode(f"{admin}:{password}".encode()).decode()


FW_AUTH = build_fw_auth(FW_ADMIN, FW_PASSWORD)
if not FW_PASSWORD:
    print(
        "WARNING: FUNKWHALE_PASSWORD not set — Funkwhale import will be skipped",
        file=sys.stderr,
    )

DJ_PLAYLISTS = {"ankd": 9, "mctoastus": 10, "momo": 11}
DISPLAY_NAMES = {"hovio": "ANKD", "mctoastus": "duck", "momo": "momo"}

RECORDINGS_DIR = os.getenv(
    "ZUKKA_RECORDINGS_DIR",
    "/var/lib/docker/volumes/azuracast_station_data/_data/zukka_radio/recordings",
)
FW_MUSIC_DIR = os.getenv("FUNKWHALE_MUSIC_DIR", "/srv/funkwhale/data/music")
STATE_FILE = os.getenv("ZUKKA_RECORDING_STATE", "/srv/azuracast/recording_state.json")
COVER_DIR = os.getenv("ZUKKA_COVER_DIR", "/opt/http/radio.zukkafabrik.de/covers")

MIN_DURATION_SEC = 120
MIN_AGE_SEC = 90  # recording must be finalized for at least 90s

if not API_KEY:
    sys.exit("ERROR: ZUKKA_RADIO_API_KEY not set")
if not TELEGRAM_TOKEN:
    sys.exit("ERROR: TELEGRAM_BOT_API_KEY not set")

API_HEADERS = {"X-API-Key": API_KEY, "Accept": "application/json"}

# ── State ─────────────────────────────────────────────────────────────


def load_state():
    try:
        with open(STATE_FILE) as f:
            return json.load(f)
    except Exception:
        return {"uploaded": {}, "started_at": None}


def save_state(s):
    os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
    with open(STATE_FILE, "w") as f:
        json.dump(s, f, indent=2)


def is_uploaded(bid):
    return str(bid) in load_state().get("uploaded", {})


def mark_uploaded(bid, info):
    s = load_state()
    s["uploaded"][str(bid)] = {
        **info,
        "uploaded_at": datetime.now(timezone.utc).isoformat(),
    }
    save_state(s)


def get_started_at():
    """Get or set the boot timestamp — only process recordings after this."""
    s = load_state()
    if s.get("started_at"):
        return s["started_at"]
    now = datetime.now(timezone.utc).isoformat()
    s["started_at"] = now
    save_state(s)
    print(f"  First run — only processing recordings after {now[:19]}")
    return now


# ── API ───────────────────────────────────────────────────────────────


def api_get(path):
    req = urllib.request.Request(f"{HOST}/api{path}", headers=API_HEADERS)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def api_put(path, data):
    body = json.dumps(data).encode()
    hdrs = {**API_HEADERS, "Content-Type": "application/json"}
    req = urllib.request.Request(
        f"{HOST}/api{path}", data=body, headers=hdrs, method="PUT"
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


# ── Naming ────────────────────────────────────────────────────────────

BERLIN_OFFSET = 2  # hours, CEST (hardcoded — no pytz dependency)


def to_berlin(dt_utc):
    """Convert UTC datetime to Europe/Berlin (CEST)."""
    from datetime import timedelta

    return dt_utc + timedelta(hours=BERLIN_OFFSET)


def get_display_name(username):
    if username in DISPLAY_NAMES:
        return DISPLAY_NAMES[username]
    return username


def format_title(dname, ts_start):
    try:
        dt = to_berlin(datetime.fromisoformat(ts_start.replace("Z", "+00:00")))
        return f"{dname} — {dt.strftime('%d.%m.%y %H:%M')}"
    except Exception:
        return f"{dname} — unknown"


def format_time_range(ts_start, ts_end):
    try:
        t1 = to_berlin(datetime.fromisoformat(ts_start.replace("Z", "+00:00")))
        t2 = to_berlin(datetime.fromisoformat(ts_end.replace("Z", "+00:00")))
        return f"{t1.strftime('%d.%m.%y')} · {t1.strftime('%H:%M')} – {t2.strftime('%H:%M')}"
    except Exception:
        return "?"


def duration_minutes(ts_start, ts_end):
    if not ts_start or not ts_end:
        return 0
    try:
        t1 = datetime.fromisoformat(ts_start.replace("Z", "+00:00"))
        t2 = datetime.fromisoformat(ts_end.replace("Z", "+00:00"))
        return max(0, int((t2 - t1).total_seconds()) // 60)
    except Exception:
        return 0


def extract_year(ts_start):
    try:
        return str(datetime.fromisoformat(ts_start.replace("Z", "+00:00")).year)
    except Exception:
        return str(datetime.now(timezone.utc).year)


def target_filename(username, recording_path):
    fn = Path(recording_path).name
    prefix = username.lower()
    if not fn.lower().startswith(f"{prefix}_"):
        fn = f"{prefix}_{fn}"
    return fn


def resolve_recording_path(recording_path, username=None):
    """Absolute path of a recording on disk.

    AzuraCast reports the path either flat ("stream_....ogg") or under a
    per-streamer subdirectory ("momo/stream_....ogg") — tolerate both.
    """
    src = os.path.join(RECORDINGS_DIR, recording_path)
    if os.path.exists(src):
        return src
    if username:
        alt = os.path.join(RECORDINGS_DIR, username.lower(), Path(recording_path).name)
        if os.path.exists(alt):
            return alt
    return src


def album_title(dname):
    return f"{dname} @ radio zukka"


# ── Telegram ──────────────────────────────────────────────────────────


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
            return json.loads(r.read()).get("ok", False)
    except Exception as e:
        print(f"  ✗ Telegram: {e}")
        return False


# ── Upload pipeline (per recording) ───────────────────────────────────


def upload_to_azuracast(b, username, dname, local_file=None):
    # Issue #4: when a post-processed master exists, upload THAT instead
    # of re-downloading the raw recording from AzuraCast.
    fn = os.path.basename(local_file) if local_file else target_filename(username, b["path"])
    title = format_title(dname, b["timestamp_start"])

    try:
        existing = api_get(
            f"/station/{STATION_ID}/files?search={urllib.parse.quote(fn)}"
        )
        if existing:
            fid = existing[0]["id"]
            pl_id = DJ_PLAYLISTS.get(username.lower(), 6)
            api_put(
                f"/station/{STATION_ID}/file/{fid}",
                {"title": title, "artist": dname, "playlists": [pl_id]},
            )
            print(f"  AzuraCast: already in media → playlist {pl_id}")
            return fn
    except Exception:
        pass

    try:
        if local_file and os.path.exists(local_file):
            with open(local_file, "rb") as f:
                data = f.read()
            print(f"  Master: {len(data) // 1024 // 1024}MB (local, no re-download)")
        else:
            req = urllib.request.Request(b["download_url"], headers={"X-API-Key": API_KEY})
            with urllib.request.urlopen(req, timeout=300) as r:
                data = r.read()
            print(f"  Download: {len(data) // 1024 // 1024}MB")
    except Exception as e:
        print(f"  ✗ Download: {e}")
        return None

    try:
        boundary = "----FW" + os.urandom(8).hex()
        ctype = "audio/mpeg" if fn.lower().endswith(".mp3") else "audio/ogg"
        body = b""
        body += f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{fn}"\r\nContent-Type: {ctype}\r\n\r\n'.encode()
        body += data
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
        fid = result.get("id")
        if not fid:
            time.sleep(2)
            found = api_get(
                f"/station/{STATION_ID}/files?search={urllib.parse.quote(fn)}"
            )
            if found:
                fid = found[0]["id"]
        if fid:
            pl_id = DJ_PLAYLISTS.get(username.lower(), 6)
            api_put(
                f"/station/{STATION_ID}/file/{fid}",
                {"title": title, "artist": dname, "playlists": [pl_id]},
            )
            print(f"  AzuraCast: ✓ → playlist {pl_id}")
            return fn
    except Exception as e:
        print(f"  ✗ Upload: {e}")
    return None


def copy_to_funkwhale(recording_path, username, source=None):
    # Issue #4: `source` is the post-processed master (already final),
    # otherwise fall back to the raw recording on disk.
    src = source or resolve_recording_path(recording_path, username)
    if not os.path.exists(src):
        print(f"  ✗ Source not found: {src}")
        return None

    fn = os.path.basename(src) if source else target_filename(username, recording_path)
    dst_dir = os.path.join(FW_MUSIC_DIR, username.lower())
    os.makedirs(dst_dir, exist_ok=True)
    dst = os.path.join(dst_dir, fn)

    if not os.path.exists(dst):
        shutil.copy2(src, dst)
        os.chmod(dst, 0o644)
        print(f"  Funkwhale: copied → {fn}")
    return dst


def reencode_ogg(filepath):
    if not shutil.which("ffmpeg"):
        return False
    tmp = filepath + ".tmp.ogg"
    try:
        subprocess.run(
            [
                "ffmpeg",
                "-y",
                "-i",
                filepath,
                "-c:a",
                "libvorbis",
                "-q:a",
                "6",
                "-map_metadata",
                "-1",
                tmp,
            ],
            check=True,
            capture_output=True,
            timeout=300,
        )
        os.replace(tmp, filepath)
        print(f"  FFmpeg: ✓")
        return True
    except Exception as e:
        print(f"  FFmpeg: {e}")
        if os.path.exists(tmp):
            os.unlink(tmp)
        return False


def tag_ogg(filepath, dname, ts_start):
    try:
        from mutagen.oggvorbis import OggVorbis

        audio = OggVorbis(filepath)
        audio["artist"] = dname
        audio["title"] = format_title(dname, ts_start)
        audio["album"] = album_title(dname)
        audio.save()
        print(f"  Mutagen: ✓")
        return True
    except Exception as e:
        print(f"  ✗ Mutagen: {e}")
        return False


def tag_file(filepath, dname, ts_start):
    """Tag the final file — OGG (vorbis) or MP3 (ID3, issue #4).

    MP3 masters from postprocess.py get ID3 tags so title/artist/album
    survive the Funkwhale import.
    """
    if str(filepath).lower().endswith(".mp3"):
        try:
            from mutagen.mp3 import MP3
            from mutagen.id3 import ID3, TALB, TIT2, TPE1

            audio = MP3(filepath)
            if audio.tags is None:
                audio.add_tags()
            audio.tags.add(TIT2(encoding=3, text=format_title(dname, ts_start)))
            audio.tags.add(TPE1(encoding=3, text=dname))
            audio.tags.add(TALB(encoding=3, text=album_title(dname)))
            audio.save()
            print(f"  Mutagen (ID3): ✓")
            return True
        except Exception as e:
            print(f"  ✗ Mutagen (ID3): {e}")
            return False
    return tag_ogg(filepath, dname, ts_start)


def funkwhale_import(filepath):
    # Music dir mounted at same path inside container (MUSIC_DIRECTORY_PATH=/srv/funkwhale/data/music)
    container_path = filepath  # same path inside container
    try:
        subprocess.run(
            [
                "docker",
                "compose",
                "-f",
                FW_COMPOSE,
                "exec",
                "-T",
                "api",
                "funkwhale-manage",
                "import_files",
                FW_LIBRARY,
                container_path,
                "--noinput",
                "--username",
                "admin",
            ],
            check=True,
            capture_output=True,
            timeout=120,
        )
        print(f"  Funkwhale: ✓ imported")
        return True
    except Exception as e:
        print(f"  ✗ Funkwhale: {e}")
        return False


def funkwhale_find_track(stem, dname):
    """Find newly imported track for a specific artist."""
    hdrs = {"Authorization": FW_AUTH}

    # 1. Look up artist ID by name
    artist_id = None
    try:
        q = urllib.parse.quote(dname)
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/artists/?page_size=5&q={q}", headers=hdrs
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            for a in json.loads(r.read()).get("results", []):
                if a["name"].lower() == dname.lower():
                    artist_id = a["id"]
                    break
    except Exception:
        pass

    if not artist_id:
        print(f"  ✗ Funkwhale: artist '{dname}' not found")
        return None

    # 2. Get most recent track for this artist, verify it matches
    try:
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/tracks/?library={FW_LIBRARY}"
            f"&artist={artist_id}&page_size=5&ordering=-creation_date",
            headers=hdrs,
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            for t in json.loads(r.read()).get("results", []):
                # Verify artist name matches (Funkwhale artist filter can be buggy)
                track_artist = (t.get("artist") or {}).get("name", "")
                if track_artist.lower() == dname.lower():
                    print(f"  Funkwhale: track #{t['id']}")
                    return t
    except Exception as e:
        print(f"  ✗ Funkwhale find: {e}")
    return None


def funkwhale_find_album(dname):
    title = album_title(dname)
    hdrs = {"Authorization": FW_AUTH}
    try:
        q = urllib.parse.quote(title)
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/albums/?library={FW_LIBRARY}&page_size=10&q={q}",
            headers=hdrs,
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            for a in json.loads(r.read()).get("results", []):
                if a["title"] == title:
                    return a
    except Exception:
        pass
    return None


def funkwhale_set_metadata(track_id, album_id, year):
    script = f"""
from funkwhale_api.music.models import Track, License, Album
track = Track.objects.get(pk={track_id})
album = Album.objects.get(pk={album_id})
lic = License.objects.get(code='cc-by-nc-sa-4.0')
track.album = album
track.license = lic
track.copyright = '(c) {year} radio zukka'
track.save()
print('OK')
"""
    try:
        subprocess.run(
            [
                "docker",
                "compose",
                "-f",
                FW_COMPOSE,
                "exec",
                "-T",
                "api",
                "funkwhale-manage",
                "shell",
                "-c",
                script,
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )
        print(f"  Funkwhale: ✓ metadata")
        return True
    except Exception as e:
        print(f"  ✗ Metadata: {e}")
        return False


def generate_cover(dname):
    os.makedirs(COVER_DIR, exist_ok=True)
    path = os.path.join(COVER_DIR, f"{dname.lower()}_album.png")
    if os.path.exists(path):
        return path
    if not shutil.which("convert"):
        return None
    try:
        subprocess.run(
            [
                "convert",
                "-size",
                "800x800",
                "xc:#0d1117",
                "-fill",
                "#f85149",
                "-draw",
                "rectangle 0,700 800,800",
                "-fill",
                "#e1e4e8",
                "-font",
                "DejaVu-Sans-Mono",
                "-pointsize",
                "48",
                "-gravity",
                "Center",
                "-annotate",
                "+0-50",
                dname,
                "-pointsize",
                "24",
                "-fill",
                "#8b949e",
                "-annotate",
                "+0+20",
                "@ radio zukka",
                path,
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )
        print(f"  Cover: ✓")
        return path
    except Exception as e:
        print(f"  Cover: {e}")
        return None


def funkwhale_upload_cover(album_id, path):
    if not path or not os.path.exists(path):
        return False
    try:
        with open(path, "rb") as f:
            data = f.read()
        boundary = "----CW" + os.urandom(8).hex()
        body = b""
        body += f'--{boundary}\r\nContent-Disposition: form-data; name="cover"; filename="cover.png"\r\nContent-Type: image/png\r\n\r\n'.encode()
        body += data
        body += f"\r\n--{boundary}--\r\n".encode()
        hdrs = {
            "Authorization": FW_AUTH,
            "Content-Type": f"multipart/form-data; boundary={boundary}",
        }
        req = urllib.request.Request(
            f"{FW_HOST}/api/v1/albums/{album_id}/",
            data=body,
            headers=hdrs,
            method="PATCH",
        )
        with urllib.request.urlopen(req, timeout=30) as r:
            print(f"  Cover: ✓ uploaded")
            return True
    except Exception as e:
        print(f"  ✗ Cover upload: {e}")
        return False


# ── Main ──────────────────────────────────────────────────────────────


def process_broadcast(b, username, dname):
    bid = b["id"]
    dur_min = duration_minutes(b["timestamp_start"], b["timestamp_end"])
    time_range = format_time_range(b["timestamp_start"], b["timestamp_end"])

    print(f"\n  #{bid} {dname} — {time_range} · {dur_min} min")

    # Issue #4: optional post-processing BEFORE upload — default OFF, so
    # nothing changes without an explicit POSTPROCESS_ENABLED=true deploy.
    # Graceful: on any failure postprocess.process() returns None and the
    # raw recording is used as before.
    local_src = resolve_recording_path(b["path"], username)
    processed = postprocess.process(local_src) if postprocess is not None else None
    if processed:
        print(f"  Postprocess: ✓ master → {os.path.basename(processed)}")

    # AzuraCast
    fn = upload_to_azuracast(b, username, dname, local_file=processed)

    # Funkwhale
    fw_path = copy_to_funkwhale(b["path"], username, source=processed)
    fw_track = None
    if fw_path:
        if processed:
            # Master is already final (mp3/opus) — do NOT re-encode to
            # vorbis, that would undo the post-processing.
            tag_file(fw_path, dname, b["timestamp_start"])
        else:
            reencode_ogg(fw_path)
            tag_ogg(fw_path, dname, b["timestamp_start"])
        if funkwhale_import(fw_path):
            time.sleep(5)  # let Funkwhale index
            stem = Path(target_filename(username, b["path"])).stem
            fw_track = funkwhale_find_track(stem, dname)
            if not fw_track:
                time.sleep(5)  # one more retry
                fw_track = funkwhale_find_track(stem, dname)
            if fw_track:
                album = funkwhale_find_album(dname)
                if album:
                    year = extract_year(b["timestamp_start"])
                    funkwhale_set_metadata(fw_track["id"], album["id"], year)
                    cover = generate_cover(dname)
                    if cover:
                        funkwhale_upload_cover(album["id"], cover)

    # Issue #4: optional original cleanup (POSTPROCESS_KEEP_ORIGINAL=0) —
    # only after AzuraCast accepted the master, never on failure.
    if (
        processed
        and postprocess is not None
        and fn
        and postprocess.should_delete_original(postprocess.load_config())
    ):
        try:
            os.remove(local_src)
            print(f"  Postprocess: removed original {os.path.basename(local_src)}")
        except Exception as e:
            print(f"  ✗ Postprocess: could not remove original: {e}")

    mark_uploaded(
        bid,
        {
            "streamer": dname,
            "title": format_title(dname, b["timestamp_start"]),
            "path": fn,
            "duration_minutes": dur_min,
            "funkwhale_track": fw_track["id"] if fw_track else None,
        },
    )

    # Telegram
    if fw_track:
        url = f"{FW_HOST}/library/tracks/{fw_track['id']}".replace("https://", "")
        msg = f"📼 {dname} — {time_range} · {dur_min} min\n\nrecording anh\u00f6ren\n{url}"
    else:
        msg = f"📼 {dname} — {time_range} · {dur_min} min"
    ok = tg_send(msg)
    print(f"  Telegram {'✓' if ok else '✗'}")


def main():
    now = datetime.now(timezone.utc)
    started_at = get_started_at()
    print(f"[{now.strftime('%H:%M:%S')}] Checking for new recordings...")

    try:
        streamers = api_get(f"/station/{STATION_ID}/streamers")
    except Exception as e:
        print(f"  API error: {e}")
        return

    new_count = 0
    for s in streamers:
        username = s["streamer_username"]
        dname = s.get("display_name") or get_display_name(username)
        try:
            blist = api_get(f"/station/{STATION_ID}/streamer/{s['id']}/broadcasts")
        except Exception:
            continue

        for b in blist:
            bid = b["id"]
            if is_uploaded(bid):
                continue

            rec = b.get("recording")
            if not rec or not rec.get("path") or rec.get("size", 0) <= 20000:
                continue

            ts_start = b.get("timestampStart")
            ts_end = b.get("timestampEnd")
            if not ts_start or not ts_end:
                continue

            # Must be finalized for at least 90s
            try:
                t_end = datetime.fromisoformat(ts_end.replace("Z", "+00:00"))
                if (now - t_end).total_seconds() < MIN_AGE_SEC:
                    continue  # too fresh, wait for next run
            except Exception:
                continue

            # Must be ≥ 2 minutes
            try:
                t1 = datetime.fromisoformat(ts_start.replace("Z", "+00:00"))
                t2 = datetime.fromisoformat(ts_end.replace("Z", "+00:00"))
                if (t2 - t1).total_seconds() < MIN_DURATION_SEC:
                    continue
            except Exception:
                continue

            # Only recordings after watcher boot (skip historical)
            if started_at:
                t_started = datetime.fromisoformat(started_at)
                if t2 < t_started:
                    continue

            process_broadcast(
                {
                    "id": bid,
                    "path": rec["path"],
                    "size": rec.get("size", 0),
                    "download_url": rec.get("downloadUrl"),
                    "timestamp_start": ts_start,
                    "timestamp_end": ts_end,
                },
                username,
                dname,
            )
            new_count += 1

    print(f"  Done. {new_count} new recording(s).")


if __name__ == "__main__":
    main()
