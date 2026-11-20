#!/bin/sh
# Test wrapper: load the service env, but post to the TEST channel.
set -a
. /etc/zukka.env
set +a
TELEGRAM_CHANNEL="$TELEGRAM_CHANNEL_TEST"
export TELEGRAM_CHANNEL
exec /usr/bin/python3 /srv/azuracast/announce.py "$@"
