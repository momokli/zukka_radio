#!/bin/sh
# Wrapper: load the service env file, then run send_photo.py with args.
set -a
. /etc/zukka.env
set +a
exec /usr/bin/python3 /srv/azuracast/send_photo.py "$@"
