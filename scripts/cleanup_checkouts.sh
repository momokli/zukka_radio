#!/bin/bash
# Remove old per-issue checkouts from the OpenClaw runners.
# Keeps the main checkouts (no "-<n>" suffix) and anything newer than N days.
set -uo pipefail

DAYS="${1:-2}"
REPOS="/home/momo/repos"
freed=0

for d in "$REPOS"/zukka_radio-* "$REPOS"/momomos-music-manager-*; do
  [ -d "$d" ] || continue
  # skip if modified within DAYS
  if [ -n "$(find "$d" -maxdepth 0 -mtime -"$DAYS" 2>/dev/null)" ]; then
    continue
  fi
  mb=$(du -sm "$d" 2>/dev/null | cut -f1)
  rm -rf "$d" && freed=$((freed + ${mb:-0}))
done

echo "$(date -Is) openclaw-cleanup: freed ${freed} MB (older than ${DAYS}d)"
