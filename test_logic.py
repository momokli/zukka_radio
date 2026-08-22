#!/usr/bin/env python3
"""
radio zukka — unit tests for pure logic functions.

Tests the naming + time formatting that caused real production bugs:
  - UTC → Berlin timezone conversion
  - title / time-range formatting
  - target_filename prefixing
  - album title / display name mapping

Run:  python3 -m pytest test_logic.py -v
  or:  python3 test_logic.py   (no deps, uses plain asserts)
"""

import importlib.util
import os
import re
import sys
from pathlib import Path

# recording_watcher.py and pipeline.py guard on these at import time
os.environ.setdefault("ZUKKA_RADIO_API_KEY", "test")
os.environ.setdefault("TELEGRAM_BOT_API_KEY", "test")

# ── Load the functions from recording_watcher.py without executing main() ──

RW = Path(__file__).parent / "recording_watcher.py"
PIPELINE = Path(__file__).parent / "pipeline.py"


def _load(module_name, path):
    spec = importlib.util.spec_from_file_location(module_name, path)
    mod = importlib.util.module_from_spec(spec)
    # Stub out anything that would run at import time
    sys.modules[module_name] = mod
    spec.loader.exec_module(mod)
    return mod


# recording_watcher imports mutagen inside a function, not at top level,
# so it's safe to import. It DOES require env vars at import time though —
# see the guard at the top of recording_watcher.py.
def test_import_safety():
    """Ensure the module exposes the functions we test."""
    rw = _load("rw", RW)
    for fn in (
        "format_title",
        "format_time_range",
        "target_filename",
        "album_title",
        "get_display_name",
    ):
        assert hasattr(rw, fn), f"recording_watcher.py missing {fn}"


def test_timezone_berlin_offset():
    """UTC timestamps must be displayed in Europe/Berlin (UTC+2 CEST)."""
    rw = _load("rw_tz", RW)
    # 08:30 UTC -> 10:30 Berlin
    assert rw.format_title("momo", "2026-06-24T08:30:00Z") == "momo — 24.06.26 10:30"


def test_time_range_berlin():
    rw = _load("rw_tr", RW)
    got = rw.format_time_range("2026-06-23T18:00:00Z", "2026-06-23T18:45:00Z")
    assert got == "23.06.26 · 20:00 – 20:45", got


def test_target_filename_prefix():
    rw = _load("rw_fn", RW)
    # already prefixed
    assert (
        rw.target_filename("mctoastus", "mctoastus_stream_20260623-140526.ogg")
        == "mctoastus_stream_20260623-140526.ogg"
    )
    # needs prefix (strips subdirectory)
    assert (
        rw.target_filename("momo", "momo/stream_20260624-092336.ogg")
        == "momo_stream_20260624-092336.ogg"
    )
    # hovio → ANKD prefix stays username-based
    assert (
        rw.target_filename("hovio", "stream_20260306-194950.ogg")
        == "hovio_stream_20260306-194950.ogg"
    )


def test_album_title():
    rw = _load("rw_al", RW)
    assert rw.album_title("duck") == "duck @ radio zukka"
    assert rw.album_title("ANKD") == "ANKD @ radio zukka"


def test_display_name_mapping():
    rw = _load("rw_dn", RW)
    # mctoastus -> duck is the critical mapping
    assert rw.get_display_name("mctoastus") == "duck"
    assert rw.get_display_name("hovio") == "ANKD"
    assert rw.get_display_name("momo") == "momo"


def test_fw_auth_from_env():
    """Funkwhale Basic auth must be built from env values, never hardcoded."""
    rw = _load("rw_fw", RW)
    assert rw.build_fw_auth("admin", "secret") == "Basic YWRtaW46c2VjcmV0"
    assert rw.build_fw_auth("admin", "") == ""  # no password → skip Funkwhale


def test_no_hardcoded_secrets_in_watcher():
    """recording_watcher.py must not contain secret-like literals."""
    src = RW.read_text()
    # long base64 blobs (a hardcoded Basic auth header would look like this)
    for m in re.finditer(r"[A-Za-z0-9+/]{40,}={0,2}", src):
        assert False, f"possible hardcoded secret in recording_watcher.py: ...{m.group(0)[:16]}..."
    # placeholder markers must not ship as real values
    assert "replace_with_" not in src


def test_pipeline_import_safety():
    """pipeline.py loads and exposes the robustness primitives."""
    pl = _load("pipeline", PIPELINE)
    for name in ("WebhookHandler", "health_payload", "ThreadingHTTPServer"):
        assert hasattr(pl, name), f"pipeline.py missing {name}"


def test_pipeline_uses_threading_server():
    """Issue #3: a hanging request must not block other webhooks."""
    from http.server import ThreadingHTTPServer as _THS

    pl = _load("pipeline_thr", PIPELINE)
    assert pl.ThreadingHTTPServer is _THS


def test_pipeline_health_payload():
    """Issue #2: /healthz payload is JSON-serializable and reports ok."""
    import json as _json

    pl = _load("pipeline_hp", PIPELINE)
    payload = pl.health_payload()
    assert payload["status"] == "ok"
    assert payload["service"] == "zukka-webhook"
    assert payload["uptime_seconds"] >= 0
    assert "time" in payload
    # must be serializable (it is served as JSON over HTTP)
    _json.dumps(payload)


if __name__ == "__main__":
    # No-dependency runner
    passed = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  ✓ {name}")
                passed += 1
            except Exception as e:
                print(f"  ✗ {name}: {e}")
    print(f"\n{passed} passed")
