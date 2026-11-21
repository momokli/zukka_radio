#!/usr/bin/env bash
# Hermetic test for the icecast config render path (issue #109).
#
# Runs docker-entrypoint.sh against the real template with test values and
# asserts:
#   1. every secret placeholder is replaced with the env value,
#   2. the admin user defaults to "admin" when unset,
#   3. no `${...}` placeholder is left in the rendered file,
#   4. a missing/empty required secret aborts with a non-zero exit.
#
# Requires only envsubst (gettext-base); no Docker, no root, no real secret.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
entrypoint="$here/docker-entrypoint.sh"
template="$here/icecast.xml.template"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
rendered="$tmp/icecast.xml"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# ── positive: all secrets set, admin user unset (defaults to admin) ──────
ICECAST_TEMPLATE="$template" ICECAST_RENDERED="$rendered" \
    ZUKKA_ICECAST_SOURCE_PASSWORD="s3cr3t-src" \
    ZUKKA_ICECAST_RELAY_PASSWORD="s3cr3t-relay" \
    ZUKKA_ICECAST_ADMIN_PASSWORD="s3cr3t-admin" \
    "$entrypoint" true

[ -f "$rendered" ] || fail "entrypoint did not render $rendered"

grep -q '<source-password>s3cr3t-src</source-password>' "$rendered" || fail "source-password not substituted"
grep -q '<relay-password>s3cr3t-relay</relay-password>' "$rendered" || fail "relay-password not substituted"
grep -q '<admin-password>s3cr3t-admin</admin-password>' "$rendered" || fail "admin-password not substituted"
grep -q '<admin-user>admin</admin-user>' "$rendered" || fail "admin-user default 'admin' not applied"

if grep -q '\${' "$rendered"; then
    fail "unsubstituted placeholder left in $rendered"
fi

# ── positive: admin user honored when set ────────────────────────────────
ICECAST_TEMPLATE="$template" ICECAST_RENDERED="$rendered" \
    ZUKKA_ICECAST_SOURCE_PASSWORD="s" \
    ZUKKA_ICECAST_RELAY_PASSWORD="r" \
    ZUKKA_ICECAST_ADMIN_PASSWORD="a" \
    ZUKKA_ICECAST_ADMIN_USER="dj" \
    "$entrypoint" true
grep -q '<admin-user>dj</admin-user>' "$rendered" || fail "admin-user override not applied"

# ── negative: each required secret missing must abort ────────────────────
missing_ok() {
    local var="$1"
    if (
        set +e
        export ICECAST_TEMPLATE="$template" ICECAST_RENDERED="$tmp/neg.xml"
        export ZUKKA_ICECAST_SOURCE_PASSWORD=s
        export ZUKKA_ICECAST_RELAY_PASSWORD=r
        export ZUKKA_ICECAST_ADMIN_PASSWORD=a
        unset "$var"
        "$entrypoint" true 2>/dev/null
    ); then
        fail "missing $var did not abort"
    fi
}

missing_ok ZUKKA_ICECAST_SOURCE_PASSWORD
missing_ok ZUKKA_ICECAST_RELAY_PASSWORD
missing_ok ZUKKA_ICECAST_ADMIN_PASSWORD

# empty value must abort too
if ICECAST_TEMPLATE="$template" ICECAST_RENDERED="$tmp/neg.xml" \
    ZUKKA_ICECAST_SOURCE_PASSWORD="" \
    ZUKKA_ICECAST_RELAY_PASSWORD=r \
    ZUKKA_ICECAST_ADMIN_PASSWORD=a \
    "$entrypoint" true 2>/dev/null; then
    fail "empty ZUKKA_ICECAST_SOURCE_PASSWORD did not abort"
fi

echo "ok   icecast render test"
