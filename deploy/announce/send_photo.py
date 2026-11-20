#!/usr/bin/env python3
"""Send a photo to a Telegram chat via the bot API.

Usage:
  send_photo.py IMAGE [--caption-file FILE] [--chat CHAT] [--token-env VAR]

Defaults: chat from $TELEGRAM_CHANNEL, token from $TELEGRAM_BOT_API_KEY.
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


def send_photo(image, chat, token, caption=None):
    boundary = "----FormBoundary" + os.urandom(16).hex()
    body = b""

    def field(name, value):
        nonlocal body
        body += (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="{name}"\r\n\r\n'
            f"{value}\r\n"
        ).encode()

    field("chat_id", chat)
    if caption:
        field("caption", caption)

    body += f"--{boundary}\r\n".encode()
    body += (
        f'Content-Disposition: form-data; name="photo"; '
        f'filename="{Path(image).name}"\r\n'
    ).encode()
    body += b"Content-Type: image/jpeg\r\n\r\n"
    body += Path(image).read_bytes()
    body += f"\r\n--{boundary}--\r\n".encode()

    req = urllib.request.Request(
        f"https://api.telegram.org/bot{token}/sendPhoto",
        data=body,
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    ap.add_argument("--caption-file")
    ap.add_argument("--chat", default=os.getenv("TELEGRAM_CHANNEL", "@zukka_radio"))
    ap.add_argument("--token-env", default="TELEGRAM_BOT_API_KEY")
    args = ap.parse_args()

    token = os.getenv(args.token_env, "")
    if not token:
        sys.exit(f"ERROR: {args.token_env} not set")

    caption = None
    if args.caption_file:
        caption = Path(args.caption_file).read_text().strip()

    try:
        res = send_photo(args.image, args.chat, token, caption)
    except urllib.error.HTTPError as e:
        sys.exit(f"HTTP {e.code}: {e.read().decode()[:400]}")

    if not res.get("ok"):
        sys.exit(f"Telegram error: {json.dumps(res)[:400]}")

    msg = res["result"]
    print(f"sent to {args.chat} — message_id={msg.get('message_id')}")


if __name__ == "__main__":
    main()
