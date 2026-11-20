#!/usr/bin/env python3
"""Scratch: test sending the poster to the TEST channel."""
import json
import os
import urllib.error
import urllib.request
from pathlib import Path

IMG = Path("/Users/momo/Downloads/einfachKaiser-pilot-poster.jpeg")

env = {}
for line in open(".env"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        env[k.strip()] = v.strip().strip('"').strip("'")

token = env.get("TELEGRAM_BOT_API_KEY_TEST") or env["TELEGRAM_BOT_API_KEY"]
chat = env.get("TELEGRAM_CHANNEL_TEST") or env["TELEGRAM_CHANNEL"]
print(f"chat: {chat!r}  token: {'test' if env.get('TELEGRAM_BOT_API_KEY_TEST') else 'prod'}")

boundary = "----B" + os.urandom(12).hex()
parts = []
parts.append(f"--{boundary}\r\nContent-Disposition: form-data; name=\"chat_id\"\r\n\r\n{chat}\r\n".encode())
parts.append(f"--{boundary}\r\nContent-Disposition: form-data; name=\"caption\"\r\n\r\nTEST POSTER\r\n".encode())
parts.append(
    f"--{boundary}\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"{IMG.name}\"\r\n"
    f"Content-Type: image/jpeg\r\n\r\n".encode()
)
parts.append(IMG.read_bytes())
parts.append(f"\r\n--{boundary}--\r\n".encode())
body = b"".join(parts)

req = urllib.request.Request(
    f"https://api.telegram.org/bot{token}/sendPhoto",
    data=body,
    headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
    method="POST",
)
try:
    with urllib.request.urlopen(req, timeout=120) as r:
        res = json.loads(r.read())
    print("ok:", res.get("ok"))
    msg = res.get("result") or {}
    print("message_id:", msg.get("message_id"))
    print("chat:", (msg.get("chat") or {}).get("title"))
except urllib.error.HTTPError as e:
    print("HTTP", e.code, e.read().decode()[:500])
