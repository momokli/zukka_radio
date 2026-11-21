#!/bin/bash
# Copy the radio Telegram bot token from the legacy stack env into the DEV env.
# Never prints the token itself.
set -euo pipefail

src=/etc/zukka.env
dst=/srv/zukka-dev/repo/env/dev.env

token=$(sudo -n grep '^TELEGRAM_BOT_API_KEY=' "$src" | cut -d= -f2-)
if [ -z "$token" ]; then
    echo "ERROR: TELEGRAM_BOT_API_KEY not found in $src" >&2
    exit 1
fi

if grep -q '^ZUKKA_TELEGRAM_BOT_TOKEN=' "$dst"; then
    sed -i "s|^ZUKKA_TELEGRAM_BOT_TOKEN=.*|ZUKKA_TELEGRAM_BOT_TOKEN=${token}|" "$dst"
else
    printf 'ZUKKA_TELEGRAM_BOT_TOKEN=%s\n' "$token" >>"$dst"
fi

len=$(grep '^ZUKKA_TELEGRAM_BOT_TOKEN=' "$dst" | cut -d= -f2- | tr -d '\n' | wc -c)
echo "ZUKKA_TELEGRAM_BOT_TOKEN length in $dst: $len"
