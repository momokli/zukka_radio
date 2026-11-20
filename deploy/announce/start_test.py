#!/usr/bin/env python3
"""Test: render and send the `start` announcement, bypassing the on-air check."""

import sys

sys.path.insert(0, "/srv/azuracast")
import announce  # noqa: E402

PLAYLIST = "scheduled-einfachkaiser"

ctx = {
    "dj": "einfachKaiser",
    "show": "einfachKaiser Show (Pilot)",
    "desc": "",
    "time": "20:00",
    "date": "20.11.26",
    "player": announce.PLAYER,
}

shows = announce.load_json(announce.SHOWS_FILE, {})
entry = shows.get(PLAYLIST) or {}
custom = (entry.get("messages") or {}).get("start")
text = announce.render_custom(custom, ctx) if custom else announce.render("start", ctx)

print("--- would send ---")
print(text)
print("------------------")
print("sent:", announce.tg_send(text))
