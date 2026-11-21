#!/bin/bash
# Remove stale per-issue checkouts and build leftovers left behind by the
# OpenClaw runners. The main checkouts (no issue suffix) are never touched.
#
#   MAX_AGE_DAYS=2  (default)
#
# Installed as ~/.local/bin/zukka-cleanup-checkouts.sh and run daily by
# zukka-cleanup.timer. Also safe to run by hand.
set -uo pipefail

MAX_AGE_DAYS="${MAX_AGE_DAYS:-2}"
LOG_TAG="zukka-cleanup"

log() { printf '%s %s: %s\n' "$(date -Is)" "$LOG_TAG" "$*"; }

freed=0

remove() {
    local path="$1"
    [ -e "$path" ] || return 0
    # Never delete something a process still holds open.
    if command -v lsof >/dev/null 2>&1 && lsof +D "$path" >/dev/null 2>&1; then
        log "skip (in use) $path"
        return 0
    fi
    local size
    size=$(du -sm "$path" 2>/dev/null | cut -f1)
    if rm -rf "$path"; then
        freed=$((freed + ${size:-0}))
        log "removed $path (${size:-?} MB)"
    else
        log "WARN could not remove $path"
    fi
}

stale() {
    [ -n "$(find "$1" -maxdepth 0 -mtime +"$MAX_AGE_DAYS" 2>/dev/null)" ]
}

# Per-issue checkouts: <repo>-<issue-number>. The bare repo names
# (momomos-music-manager, zukka_radio, riftbreaker-battle-mod, …) have no
# trailing issue number and are therefore never matched.
for base in /home/momo/repos/*-[0-9]*; do
    [ -d "$base" ] || continue
    stale "$base" && remove "$base"
done

# Build/review leftovers in /tmp and /srv/tmp. Only known runner patterns;
# system dirs (.X11-unix, systemd-private-*, …) are never matched.
for base in /tmp/pr* /tmp/mmm-* /tmp/mmr* /tmp/mh-* /tmp/zukka-* /tmp/dev*-target \
    /srv/tmp/*-target /srv/zukka-build-*; do
    [ -e "$base" ] || continue
    stale "$base" && remove "$base"
done

# Docker: dangling images and build cache older than 7 days. Running
# containers are never affected (image prune only drops unused images).
if command -v docker >/dev/null 2>&1; then
    docker image prune -af --filter until=168h >/dev/null 2>&1 || true
    docker builder prune -af --filter until=168h >/dev/null 2>&1 || true
fi

log "summary freed_mb=$freed"
df -h / | tail -1
