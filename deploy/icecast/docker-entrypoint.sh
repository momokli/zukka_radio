#!/bin/sh
# Icecast entrypoint: render the config template, then start icecast.
#
# Icecast 2.4.4 does not expand environment variables in its XML config, so
# the image ships icecast.xml.template and this entrypoint renders the three
# secrets (plus the admin user) into /etc/icecast2/icecast.xml at start time
# using envsubst (gettext-base).
#
# Fail-fast: a missing/empty secret aborts the container. There is no default
# password in the image or the repository.
set -eu

template="/etc/icecast2/icecast.xml.template"
rendered="/etc/icecast2/icecast.xml"

# Admin user is not a secret; default to "admin" when unset/empty.
# envsubst cannot express a default, so apply it here.
if [ -z "${ZUKKA_ICECAST_ADMIN_USER:-}" ]; then
    ZUKKA_ICECAST_ADMIN_USER="admin"
fi
export ZUKKA_ICECAST_ADMIN_USER

if [ -z "${ZUKKA_ICECAST_SOURCE_PASSWORD:-}" ]; then
    echo "icecast-entrypoint: required environment variable ZUKKA_ICECAST_SOURCE_PASSWORD is unset or empty" >&2
    exit 1
fi
if [ -z "${ZUKKA_ICECAST_RELAY_PASSWORD:-}" ]; then
    echo "icecast-entrypoint: required environment variable ZUKKA_ICECAST_RELAY_PASSWORD is unset or empty" >&2
    exit 1
fi
if [ -z "${ZUKKA_ICECAST_ADMIN_PASSWORD:-}" ]; then
    echo "icecast-entrypoint: required environment variable ZUKKA_ICECAST_ADMIN_PASSWORD is unset or empty" >&2
    exit 1
fi

# Scope envsubst to exactly these variables so no other `$` sequence in the
# template is touched.
envsubst '${ZUKKA_ICECAST_SOURCE_PASSWORD} ${ZUKKA_ICECAST_RELAY_PASSWORD} ${ZUKKA_ICECAST_ADMIN_USER} ${ZUKKA_ICECAST_ADMIN_PASSWORD}' \
    <"$template" >"$rendered"

exec "$@"
