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
import tempfile
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


# ── Issue #4: postprocess.py (pure logic, no audio binaries) ──────────

PP = Path(__file__).parent / "postprocess.py"


def _load_pp(name):
    """Load postprocess.py as a fresh module (no env, no ffmpeg needed)."""
    return _load(name, PP)


def test_postprocess_import_safety():
    """postprocess.py loads without env vars or binaries and exposes the API."""
    pp = _load_pp("pp_imp")
    for fn in (
        "load_config",
        "is_enabled",
        "should_delete_original",
        "compute_gain",
        "build_filter_chain",
        "build_encoder_args",
        "build_ffmpeg_args",
        "output_path",
        "parse_ebur128_output",
        "measure_loudness",
        "probe_duration",
        "process",
    ):
        assert hasattr(pp, fn), f"postprocess.py missing {fn}"


def test_postprocess_disabled_by_default():
    """Default is OFF — nothing active without an explicit deploy (#4)."""
    pp = _load_pp("pp_dis")
    cfg = pp.load_config({})
    assert pp.is_enabled(cfg) is False
    # safe defaults
    assert cfg["target_lufs"] == -16.0
    assert cfg["target_tp"] == -1.5
    assert cfg["format"] == "mp3"
    assert cfg["bitrate"] == "320k"
    assert cfg["keep_original"] is True


def test_postprocess_enable_flag():
    """POSTPROCESS_ENABLED must be explicitly set to activate."""
    pp = _load_pp("pp_en")
    assert pp.is_enabled(pp.load_config({"POSTPROCESS_ENABLED": "1"})) is True
    assert pp.is_enabled(pp.load_config({"POSTPROCESS_ENABLED": "true"})) is True
    assert pp.is_enabled(pp.load_config({"POSTPROCESS_ENABLED": "0"})) is False
    assert pp.is_enabled(pp.load_config({"POSTPROCESS_ENABLED": "false"})) is False
    assert pp.is_enabled(pp.load_config({})) is False


def test_postprocess_config_parsing():
    """Env parsing: numbers, formats, bitrates; bad values fall back."""
    pp = _load_pp("pp_cfg")
    cfg = pp.load_config(
        {
            "TARGET_LUFS": "-14.5",
            "TARGET_TP": "-1.0",
            "FORMAT": "OPUS",
            "BITRATE": "96k",
        }
    )
    assert cfg["target_lufs"] == -14.5
    assert cfg["target_tp"] == -1.0
    assert cfg["format"] == "opus"
    assert cfg["bitrate"] == "96k"
    # unknown format → mp3, garbage number → default
    bad = pp.load_config({"FORMAT": "flac", "TARGET_LUFS": "loud"})
    assert bad["format"] == "mp3"
    assert bad["target_lufs"] == -16.0


def test_compute_gain_pure():
    """Static gain = min(loudness correction, true-peak headroom)."""
    pp = _load_pp("pp_cg")
    # -20 LUFS → -16: +4 dB, TP -2.0 → -1.5 allows +0.5 → min = +0.5
    assert pp.compute_gain(-20.0, -2.0, -16.0, -1.5) == 0.5
    # TP far below target → loudness correction wins
    assert pp.compute_gain(-20.0, -6.0, -16.0, -1.5) == 4.0
    # already louder than target → negative gain
    assert pp.compute_gain(-12.0, -3.0, -16.0, -1.5) == -4.0
    # unknown measurements → 0 dB (nothing to normalize against)
    assert pp.compute_gain(None, None, -16.0, -1.5) == 0.0
    # unknown TP → loudness gain applied, no TP cap (limiter backs it up)
    assert pp.compute_gain(-20.0, None, -16.0, -1.5) == 4.0
    # unknown LUFS + hot master → TP-safe attenuation only, never boost
    assert pp.compute_gain(None, -0.5, -16.0, -1.5) == -1.0


def test_filter_chain_default():
    """Chain: highpass first, alimiter last; no extras when unconfigured."""
    pp = _load_pp("pp_fc")
    cfg = pp.load_config({})
    chain = pp.build_filter_chain(cfg)
    assert chain[0] == "highpass=f=30"
    assert chain[-1].startswith("alimiter=")
    assert all("volume=" not in f for f in chain)  # no measurement → 0 dB
    assert all("acompressor" not in f for f in chain)
    assert all("afade" not in f for f in chain)


def test_filter_chain_gain_and_limiter():
    """Static gain inserted between highpass and alimiter; TP → limit."""
    pp = _load_pp("pp_fcg")
    cfg = pp.load_config({"TARGET_LUFS": "-16", "TARGET_TP": "-1.5"})
    chain = pp.build_filter_chain(cfg, measured_lufs=-20.0, measured_tp=-4.0)
    assert chain[0] == "highpass=f=30"
    # -20 LUFS → -16: +4 dB; TP -4.0 → -1.5 allows +2.5 → min = +2.5
    assert "volume=2.50dB" in chain
    # -1.5 dBTP → linear limit 10^(-1.5/20) = 0.841395
    assert "alimiter=level=false:limit=0.841395" in chain
    assert chain.index("volume=2.50dB") < chain.index("alimiter=level=false:limit=0.841395")


def test_filter_chain_compressor_and_fades():
    """Optional compressor + fades only when configured."""
    pp = _load_pp("pp_fcf")
    cfg = pp.load_config(
        {
            "POSTPROCESS_COMPRESSOR": "1",
            "POSTPROCESS_FADE_IN": "2",
            "POSTPROCESS_FADE_OUT": "5",
        }
    )
    chain = pp.build_filter_chain(cfg, duration=3600.0)
    assert any(f.startswith("acompressor=") for f in chain)
    assert "afade=t=in:st=0:d=2.000" in chain
    assert "afade=t=out:st=3595.000:d=5.000" in chain
    # fade-out skipped when duration unknown (graceful)
    chain_no_dur = pp.build_filter_chain(cfg, duration=None)
    assert not any("afade=t=out" in f for f in chain_no_dur)


def test_build_ffmpeg_args_mp3():
    """MP3 output: libmp3lame, -b:a 320k, filters as single -af arg."""
    pp = _load_pp("pp_args")
    cfg = pp.load_config({})
    args = pp.build_ffmpeg_args(cfg, "in.ogg", "out.mp3")
    assert args[0] == cfg["ffmpeg"] and args[1] == "-y"
    assert args[args.index("-i") + 1] == "in.ogg"
    assert args[args.index("-af") + 1] == "highpass=f=30,alimiter=level=false:limit=0.841395"
    assert args[args.index("-c:a") + 1] == "libmp3lame"
    assert args[args.index("-b:a") + 1] == "320k"
    assert args[-1] == "out.mp3"


def test_build_ffmpeg_args_opus():
    """Opus output: libopus encoder with configured bitrate."""
    pp = _load_pp("pp_opus")
    cfg = pp.load_config({"FORMAT": "opus", "BITRATE": "128k"})
    args = pp.build_ffmpeg_args(cfg, "in.ogg", "out.opus")
    assert args[args.index("-c:a") + 1] == "libopus"
    assert args[args.index("-b:a") + 1] == "128k"
    assert args[-1] == "out.opus"


def test_output_path_suffix():
    """Destination: same stem + FORMAT suffix, next to the source by default."""
    pp = _load_pp("pp_op")
    assert pp.output_path("rec/momo/stream_20260623-140526.ogg", pp.load_config({})) == Path(
        "rec/momo/stream_20260623-140526.mp3"
    )
    assert pp.output_path("rec/x.ogg", pp.load_config({"FORMAT": "opus"})) == Path(
        "rec/x.opus"
    )
    cfg = pp.load_config({"POSTPROCESS_OUTPUT_DIR": "/tmp/out"})
    assert pp.output_path("rec/x.ogg", cfg) == Path("/tmp/out/x.mp3")


def test_parse_ebur128_output():
    """Parse the ffmpeg ebur128 summary (stderr text) into floats."""
    pp = _load_pp("pp_eb")
    text = (
        "[Parsed_ebur128_0 @ 0x55] Summary:\n"
        "[Parsed_ebur128_0 @ 0x55] Integrated loudness:\n"
        "    I:         -18.4 LUFS\n"
        "    Threshold: -28.4 LUFS\n"
        "[Parsed_ebur128_0 @ 0x55] Loudness range:\n"
        "    LRA:         6.0 LU\n"
        "[Parsed_ebur128_0 @ 0x55] True peak:\n"
        "    Peak:       -1.2 dBTP\n"
    )
    parsed = pp.parse_ebur128_output(text)
    assert parsed["integrated_lufs"] == -18.4
    assert parsed["true_peak_db"] == -1.2
    empty = pp.parse_ebur128_output("no summary here")
    assert empty["integrated_lufs"] is None
    assert empty["true_peak_db"] is None


def test_process_disabled_never_runs_ffmpeg():
    """Disabled module: process() returns None without touching binaries."""
    pp = _load_pp("pp_pdis")
    with tempfile.TemporaryDirectory() as td:
        src = Path(td) / "in.ogg"
        src.write_bytes(b"x")
        calls = []

        def fake_run(cmd, timeout):
            calls.append(list(cmd))
            return "", ""

        cfg = pp.load_config({})  # POSTPROCESS_ENABLED unset
        assert pp.process(str(src), config=cfg, run_cmd=fake_run) is None
        assert calls == []  # ffmpeg never invoked


def test_process_full_pipeline_fake_runner():
    """Enabled: measure → encode; gain applied, master produced."""
    pp = _load_pp("pp_pfull")
    with tempfile.TemporaryDirectory() as td:
        src = Path(td) / "in.ogg"
        src.write_bytes(b"x")
        calls = []

        def fake_run(cmd, timeout):
            calls.append(list(cmd))
            if "ebur128" in cmd:
                # -20 LUFS, TP -6.0 has headroom for the full +4 dB gain
                return "I: -20.0 LUFS\nPeak: -6.0 dBTP\n", ""
            if "ffprobe" in cmd:
                return "1800.0\n", ""
            Path(cmd[-1]).write_bytes(b"fake master")
            return "", ""

        cfg = pp.load_config({"POSTPROCESS_ENABLED": "1"})
        dst = pp.process(str(src), config=cfg, run_cmd=fake_run)
        assert dst == str(Path(td) / "in.mp3")
        assert Path(dst).is_file()
        assert len(calls) == 2  # measure + encode (no ffprobe w/o fades)
        af = calls[1][calls[1].index("-af") + 1]
        assert "volume=4.00dB" in af
        assert "alimiter=level=false:limit=0.841395" in af
        assert calls[1][calls[1].index("-b:a") + 1] == "320k"


def test_process_graceful_when_measure_fails():
    """Measurement failure must not break the run — 0 dB gain fallback."""
    pp = _load_pp("pp_pg")
    with tempfile.TemporaryDirectory() as td:
        src = Path(td) / "in.ogg"
        src.write_bytes(b"x")
        calls = []

        def fake_run(cmd, timeout):
            calls.append(list(cmd))
            if "ebur128" in cmd:
                raise RuntimeError("ffmpeg binary missing")
            Path(cmd[-1]).write_bytes(b"x")
            return "", ""

        cfg = pp.load_config({"POSTPROCESS_ENABLED": "1"})
        dst = pp.process(str(src), config=cfg, run_cmd=fake_run)
        assert dst is not None  # still processed
        af = calls[1][calls[1].index("-af") + 1]
        assert "volume=" not in af  # 0 dB gain fallback
        assert "alimiter=level=false:limit=0.841395" in af


def test_process_fade_out_uses_probed_duration():
    """Fade-out needs duration: ffprobe is called, fade math applied."""
    pp = _load_pp("pp_pf")
    with tempfile.TemporaryDirectory() as td:
        src = Path(td) / "in.ogg"
        src.write_bytes(b"x")
        calls = []

        def fake_run(cmd, timeout):
            calls.append(list(cmd))
            if "ebur128" in cmd:
                return "I: -16.0 LUFS\nPeak: -3.0 dBTP\n", ""
            if "ffprobe" in cmd:
                return "3600.0\n", ""
            Path(cmd[-1]).write_bytes(b"x")
            return "", ""

        cfg = pp.load_config(
            {"POSTPROCESS_ENABLED": "1", "POSTPROCESS_FADE_OUT": "5"}
        )
        dst = pp.process(str(src), config=cfg, run_cmd=fake_run)
        assert dst is not None
        assert len(calls) == 3  # measure + probe + encode
        af = calls[2][calls[2].index("-af") + 1]
        assert "afade=t=out:st=3595.000:d=5.000" in af


def test_process_encode_failure_is_graceful():
    """Encode failure → None, partial output cleaned up, no exception."""
    pp = _load_pp("pp_pe")
    with tempfile.TemporaryDirectory() as td:
        src = Path(td) / "in.ogg"
        src.write_bytes(b"x")

        def fake_run(cmd, timeout):
            if "ebur128" in cmd:
                return "I: -16.0 LUFS\nPeak: -3.0 dBTP\n", ""
            raise RuntimeError("encoder exploded")

        cfg = pp.load_config({"POSTPROCESS_ENABLED": "1"})
        assert pp.process(str(src), config=cfg, run_cmd=fake_run) is None
        assert not (Path(td) / "in.mp3").exists()  # partial output removed


def test_keep_original_config():
    """Original deletion is opt-in via POSTPROCESS_KEEP_ORIGINAL=0."""
    pp = _load_pp("pp_ko")
    assert pp.should_delete_original(pp.load_config({})) is False
    assert (
        pp.should_delete_original(pp.load_config({"POSTPROCESS_KEEP_ORIGINAL": "0"}))
        is True
    )
    assert (
        pp.should_delete_original(pp.load_config({"POSTPROCESS_KEEP_ORIGINAL": "false"}))
        is True
    )


def test_watcher_postprocess_integration():
    """recording_watcher imports postprocess and exposes the hook helpers."""
    rw = _load("rw_pp", RW)
    assert rw.postprocess is not None
    assert hasattr(rw.postprocess, "process")
    assert hasattr(rw, "resolve_recording_path")
    assert hasattr(rw, "tag_file")
    # hook is inert by default: disabled config → process returns None
    assert rw.postprocess.process("whatever.ogg") is None


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
