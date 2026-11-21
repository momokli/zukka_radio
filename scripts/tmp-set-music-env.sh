#!/bin/bash
# Runs on planet: point DEV at the real Music API/Hub and install the token.
set -euo pipefail

f=/srv/zukka-dev/repo/env/dev.env
[ -s /tmp/ma-token ] || { echo "ERROR: /tmp/ma-token missing" >&2; exit 1; }
TOKEN=$(cat /tmp/ma-token)

sed -i "s|^ZUKKA_MUSIC_API_URL=.*|ZUKKA_MUSIC_API_URL=https://api.zukkafabrik.de|" "$f"
sed -i "s|^ZUKKA_MUSIC_HUB_URL=.*|ZUKKA_MUSIC_HUB_URL=https://hub.zukkafabrik.de|" "$f"
sed -i "s|^ZUKKA_MUSIC_API_TOKEN=.*|ZUKKA_MUSIC_API_TOKEN=${TOKEN}|" "$f"
rm -f /tmp/ma-token

grep -E '^ZUKKA_MUSIC_(API_URL|HUB_URL)=' "$f"
len=$(grep '^ZUKKA_MUSIC_API_TOKEN=' "$f" | cut -d= -f2- | tr -d '\n' | wc -c)
echo "token_len=${len}"
