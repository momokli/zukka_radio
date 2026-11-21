#!/bin/bash
# Remove old per-issue checkouts left behind by the OpenClaw runners.
# Keeps the main checkouts (zukka_radio, momomos-music-manager) untouched.
set -uo pipefail

MAX_AGE_DAYS="${MAX_AGE_DAYS:-2}"
LOG_TAG="zukka-cleanup"

log() { printf '%s %s: %s\n' "$(date -Is)" "$LOG_TAG" "$*"; }

freed=0
for base in /home/momo/repos/zukka_radio-* /home/momo/repos/momomos-music-manager-*; do
  [ -d "$base" ] || continue
  # Only dirs whose mtime is older than MAX_AGE_DAYS.
  if [ -n "$(find "$base" -maxdepth 0 -mtime +"$MAX_AGE_DAYS" 2>/dev/null)" ]; then
    size=$(du -sm "$base" 2>/dev/null | cut -f1)
    if rm -rf "$base"; then
      freed=$((freed + ${size:-0}))
      log "removed $base (${size:-?} MB)"
    else
      log "WARN could not remove $base"
    fi
  fi
done

log "summary freed_mb=$freed"
df -h / | tail -1
