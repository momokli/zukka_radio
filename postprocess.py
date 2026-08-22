#!/usr/bin/env python3
"""
radio zukka — Recording Post-Processor (issue #4)
==================================================
Standalone, modular ffmpeg filter chain applied AFTER a broadcast is
finalized and BEFORE it is uploaded to AzuraCast / Funkwhale.

Design ("pipeline style"):
  - Pure functions build the ffmpeg command line (fully unit-testable
    without any audio binaries): load_config, compute_gain,
    build_filter_chain, build_ffmpeg_args, output_path,
    parse_ebur128_output.
  - A thin execution layer (measure_loudness / probe_duration / process)
    runs ffmpeg/ffprobe via subprocess.
  - Always graceful: any failure (missing binary, measurement error,
    encode error) logs and returns None — callers keep the original file.
    The module is inert unless POSTPROCESS_ENABLED is explicitly set.

Chain (SOTA order, every stage optional):
  1. highpass=f=30                  — rumble / DC removal
  2. (optional) acompressor         — dynamic control
  3. static loudness normalization  — EBU R128 measured, CONSTANT gain
                                      (no dynamic loudnorm that pumps):
                                      target TARGET_LUFS (default -16 LUFS),
                                      true peak capped at TARGET_TP
                                      (default -1.5 dBTP)
  4. alimiter                       — safety limiter, no auto-level
  5. (optional) afade in/out        — broadcast fades

Output:
  MP3 320 kbps CBR via libmp3lame (`-b:a 320k`). CBR + Xing header
  (written automatically by ffmpeg >= 4.0's libmp3lame encoder) gives
  correct duration + perfect seeking in browsers and works on Safari.
  Opus optional via FORMAT=opus.

Config (env, all read at call time — no import-time side effects):

  POSTPROCESS_ENABLED      0/1 — master switch, default 0 (disabled)
  TARGET_LUFS              loudness target, default -16 (LUFS)
  TARGET_TP                true-peak ceiling, default -1.5 (dBTP)
  FORMAT                   mp3 (default) | opus
  BITRATE                  default 320k (mp3) / 128k (opus)
  POSTPROCESS_COMPRESSOR   0/1 — optional compressor stage, default 0
  POSTPROCESS_FADE_IN      seconds, default 0 (off)
  POSTPROCESS_FADE_OUT     seconds, default 0 (off)
  POSTPROCESS_KEEP_ORIGINAL 0/1 — delete source after success, default 1 (keep)
  POSTPROCESS_OUTPUT_DIR   output dir override (default: next to source)
  POSTPROCESS_FFMPEG / POSTPROCESS_FFPROBE  binary overrides (default ffmpeg/ffprobe)
"""

import os
import re
import subprocess
from pathlib import Path

# ── Constants ─────────────────────────────────────────────────────────

SUFFIXES = {"mp3": ".mp3", "opus": ".opus"}

_LUFS_RE = re.compile(r"I:\s+(-?\d+(?:\.\d+)?)\s+LUFS")
_TP_RE = re.compile(r"Peak:\s+(-?\d+(?:\.\d+)?)\s+dBTP")


class PostprocessError(RuntimeError):
    """Raised by the real subprocess runner on command failure."""


# ── Config (pure) ─────────────────────────────────────────────────────


def _as_bool(value, default=False):
    if value is None:
        return default
    return str(value).strip().lower() in ("1", "true", "yes", "on")


def _as_float(value, default):
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def load_config(env=None):
    """Read configuration from the environment (or a dict, for tests).

    Pure: no I/O, no side effects. Never raises on bad values — bad
    numbers fall back to defaults, bad FORMAT falls back to mp3.
    """
    env = os.environ if env is None else env
    fmt = str(env.get("FORMAT", "mp3")).strip().lower()
    if fmt not in SUFFIXES:
        fmt = "mp3"
    bitrate = env.get("BITRATE") or ("320k" if fmt == "mp3" else "128k")
    cfg = {
        "enabled": _as_bool(env.get("POSTPROCESS_ENABLED")),
        "target_lufs": _as_float(env.get("TARGET_LUFS"), -16.0),
        "target_tp": _as_float(env.get("TARGET_TP"), -1.5),
        "format": fmt,
        "bitrate": str(bitrate).strip(),
        "compressor": _as_bool(env.get("POSTPROCESS_COMPRESSOR")),
        "fade_in": _as_float(env.get("POSTPROCESS_FADE_IN"), 0.0),
        "fade_out": _as_float(env.get("POSTPROCESS_FADE_OUT"), 0.0),
        "keep_original": _as_bool(env.get("POSTPROCESS_KEEP_ORIGINAL"), True),
        "output_dir": env.get("POSTPROCESS_OUTPUT_DIR") or None,
        "ffmpeg": env.get("POSTPROCESS_FFMPEG", "ffmpeg"),
        "ffprobe": env.get("POSTPROCESS_FFPROBE", "ffprobe"),
        # internal (not env-configurable): generous ceilings for long sets
        "measure_timeout": 1800,
        "encode_timeout": 3600,
    }
    return cfg


def is_enabled(config):
    """Master switch — module is inert unless POSTPROCESS_ENABLED is set."""
    return bool(config.get("enabled"))


def should_delete_original(config):
    """True when the caller may remove the source after a successful run."""
    return not bool(config.get("keep_original", True))


def output_path(src, config):
    """Destination path: same stem as src, suffix from FORMAT.

    Default: same directory as the source (keeps the file next to the
    raw recording, so the Funkwhale copy step can pick it up), unless
    POSTPROCESS_OUTPUT_DIR overrides it.
    """
    src = Path(src)
    out_dir = Path(config["output_dir"]) if config.get("output_dir") else src.parent
    return out_dir / f"{src.stem}{SUFFIXES[config['format']]}"


# ── Loudness math (pure) ──────────────────────────────────────────────


def compute_gain(measured_lufs, measured_tp, target_lufs=-16.0, target_tp=-1.5,
                 max_gain_db=12.0):
    """Constant gain (dB) to reach target LUFS without exceeding TP.

    Static normalization: gain is the *minimum* of the loudness
    correction and the true-peak headroom, so the alimiter only ever
    catches the few remaining transients instead of doing heavy lifting.
    Unknown measurements (None) contribute 0 dB.

    >>> compute_gain(-20.0, -2.0, -16.0, -1.5)   # need +4 LUFS, TP allows +0.5
    0.5
    >>> compute_gain(-20.0, -4.0, -16.0, -1.5)   # need +4 LUFS, TP allows +2.5
    4.0
    """
    gain_lufs = target_lufs - measured_lufs if measured_lufs is not None else 0.0
    # unknown true peak → no TP constraint (limiter still catches peaks)
    gain_tp = (
        target_tp - measured_tp if measured_tp is not None else float("inf")
    )
    gain = min(gain_lufs, gain_tp)
    return max(-max_gain_db, min(max_gain_db, gain))


# ── Filter chain + ffmpeg args (pure) ─────────────────────────────────


def build_filter_chain(config, measured_lufs=None, measured_tp=None, duration=None):
    """Ordered audio filter chain (one `-af` argument, comma-joined).

    Pure — no subprocess. Stages:
      highpass → (compressor) → volume (static gain) → alimiter → (fades)
    """
    filters = ["highpass=f=30"]

    if config.get("compressor"):
        filters.append("acompressor=threshold=-18dB:ratio=3:attack=20:release=250")

    gain = compute_gain(
        measured_lufs, measured_tp,
        config["target_lufs"], config["target_tp"],
    )
    if gain != 0.0:
        filters.append(f"volume={gain:.2f}dB")

    # Safety limiter: pure brickwall at TARGET_TP, auto-level OFF so it
    # never colors the static normalization.
    tp_linear = 10 ** (config["target_tp"] / 20.0)
    filters.append(f"alimiter=level=false:limit={tp_linear:.6f}")

    if config.get("fade_in"):
        filters.append(f"afade=t=in:st=0:d={config['fade_in']:.3f}")
    # fade-out needs the total duration; skipped when unknown
    if config.get("fade_out") and duration:
        out_start = max(0.0, duration - config["fade_out"])
        filters.append(f"afade=t=out:st={out_start:.3f}:d={config['fade_out']:.3f}")

    return filters


def build_encoder_args(config):
    """Codec args for the output format."""
    if config["format"] == "opus":
        return ["-c:a", "libopus", "-b:a", config["bitrate"]]
    # MP3 320 kbps CBR (libmp3lame): fixed -b:a → CBR with Xing/Info
    # header (ffmpeg >= 4.0 writes it automatically) → correct duration,
    # seeking and Safari support.
    return ["-c:a", "libmp3lame", "-b:a", config["bitrate"]]


def build_ffmpeg_args(config, src, dst, measured_lufs=None, measured_tp=None,
                      duration=None):
    """Full argv for the encode pass. Pure — fully unit-testable."""
    args = [config["ffmpeg"], "-y", "-i", str(src), "-vn"]
    filters = build_filter_chain(config, measured_lufs, measured_tp, duration)
    if filters:
        args += ["-af", ",".join(filters)]
    args += build_encoder_args(config)
    args += ["-map_metadata", "0", str(dst)]
    return args


def parse_ebur128_output(text):
    """Parse the `ffmpeg -af ebur128 -f null -` summary.

    Returns {"integrated_lufs": float|None, "true_peak_db": float|None}.
    Pure. Missing values → None (caller falls back to 0 dB gain).
    """
    m = _LUFS_RE.search(text)
    tp = _TP_RE.search(text)
    return {
        "integrated_lufs": float(m.group(1)) if m else None,
        "true_peak_db": float(tp.group(1)) if tp else None,
    }


# ── Execution layer (thin, graceful) ──────────────────────────────────


def _run(cmd, run_cmd=None, timeout=600):
    """Run a command and return (stdout, stderr).

    `run_cmd` is an injectable runner for tests: callable(cmd, timeout)
    → (stdout, stderr). Default: subprocess; raises PostprocessError on
    non-zero exit.
    """
    if run_cmd is not None:
        return run_cmd(cmd, timeout)
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except FileNotFoundError as e:
        raise PostprocessError(f"binary not found: {cmd[0]}") from e
    except subprocess.TimeoutExpired as e:
        raise PostprocessError(f"timed out after {timeout}s: {cmd[0]}") from e
    if p.returncode != 0:
        raise PostprocessError(
            f"command failed (rc={p.returncode}): {cmd[0]} "
            f"… stderr: {p.stderr[-500:]}"
        )
    return p.stdout or "", p.stderr or ""


def measure_loudness(src, config=None, run_cmd=None):
    """EBU R128 measurement pass. Returns parsed dict or None on failure."""
    config = load_config() if config is None else config
    cmd = [
        config["ffmpeg"], "-nostats", "-i", str(src),
        "-af", "ebur128", "-f", "null", "-",
    ]
    try:
        out, err = _run(cmd, run_cmd, timeout=config.get("measure_timeout", 1800))
        parsed = parse_ebur128_output(out + "\n" + err)
    except Exception as e:
        print(f"  Postprocess: ⚠ loudness measurement failed ({e})")
        return None
    if parsed["integrated_lufs"] is None:
        print("  Postprocess: ⚠ no ebur128 summary found — using 0 dB gain")
        return None
    return parsed


def probe_duration(src, config=None, run_cmd=None):
    """Duration in seconds via ffprobe, or None on failure."""
    config = load_config() if config is None else config
    cmd = [
        config["ffprobe"], "-v", "error",
        "-show_entries", "format=duration",
        "-of", "default=noprint_wrappers=1:nokey=1",
        str(src),
    ]
    try:
        out, _ = _run(cmd, run_cmd, timeout=60)
        return float(out.strip())
    except Exception as e:
        print(f"  Postprocess: ⚠ duration probe failed ({e})")
        return None


def process(src, config=None, run_cmd=None):
    """Post-process a finalized recording: src → mp3/opus master.

    Returns the destination path (str) on success, None when disabled
    or on ANY failure — callers then keep using the original file.
    Never raises; always logs to stdout (journald).
    """
    config = load_config() if config is None else config
    if not is_enabled(config):
        return None
    src = Path(src)
    if not src.is_file():
        print(f"  Postprocess: ✗ source not found: {src}")
        return None

    dst = output_path(src, config)
    if dst == src:
        print(f"  Postprocess: ✗ output would overwrite source: {dst}")
        return None

    # 1. Static loudness measurement (EBU R128) — graceful: 0 dB on failure
    loudness = measure_loudness(src, config, run_cmd)
    measured_lufs = loudness["integrated_lufs"] if loudness else None
    measured_tp = loudness["true_peak_db"] if loudness else None

    # 2. Duration — only needed for fade-out; graceful: fades skipped
    duration = None
    if config.get("fade_out"):
        duration = probe_duration(src, config, run_cmd)
        if duration is None:
            print("  Postprocess: ⚠ skipping fade-out (duration unknown)")

    # 3. Build + run the encode pass
    args = build_ffmpeg_args(
        config, src, dst, measured_lufs, measured_tp, duration
    )
    print(f"  Postprocess: ffmpeg {' '.join(str(a) for a in args)}")
    try:
        _run(args, run_cmd, timeout=config.get("encode_timeout", 3600))
    except Exception as e:
        print(f"  Postprocess: ✗ encode failed ({e})")
        dst.unlink(missing_ok=True)
        return None

    if not dst.is_file():
        print(f"  Postprocess: ✗ no output produced: {dst}")
        return None

    return str(dst)


if __name__ == "__main__":
    import sys

    # Manual CLI (debug/verify): python3 postprocess.py <file> [--noop]
    cfg = load_config()
    print(f"enabled={cfg['enabled']} format={cfg['format']} "
          f"bitrate={cfg['bitrate']} lufs={cfg['target_lufs']} "
          f"tp={cfg['target_tp']}dBTP")
    if len(sys.argv) < 2:
        sys.exit("usage: postprocess.py <recording>")
    out = process(sys.argv[1], config=cfg)
    print(f"→ {out or '(disabled or failed — original kept)'}")
