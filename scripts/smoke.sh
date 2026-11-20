#!/usr/bin/env bash
# Post-deploy smoke test.
#
#   ./scripts/smoke.sh dev
#
# Runs on the deploy host: all endpoints are checked through the loopback
# listeners (DEV block 121xx). Every check retries until ZUKKA_SMOKE_TIMEOUT
# seconds (default 30) have elapsed, then the script exits non-zero.
set -euo pipefail

env_name="${1:-}"
if [[ "$env_name" != "dev" ]]; then
    if [[ "$env_name" == "prod" ]]; then
        echo "smoke: prod is out of scope for #40 (see #41)" >&2
    else
        echo "usage: $0 <dev>" >&2
    fi
    exit 2
fi

caddy_https="${ZUKKA_SMOKE_CADDY_HTTPS:-https://127.0.0.1:12180}"
caddy_admin="${ZUKKA_SMOKE_CADDY_ADMIN:-http://127.0.0.1:12181}"
scheduler_base="${ZUKKA_SMOKE_SCHEDULER:-http://127.0.0.1:12110}"

domain_radio="${ZUKKA_DOMAIN_RADIO:-radio.dev.zukkafabrik.de}"
domain_portal="${ZUKKA_DOMAIN_PORTAL:-portal.dev.zukkafabrik.de}"
domain_tapes="${ZUKKA_DOMAIN_TAPES:-tapes.dev.zukkafabrik.de}"

timeout_secs="${ZUKKA_SMOKE_TIMEOUT:-30}"
deadline=$((SECONDS + timeout_secs))

check() {
    local name="$1"
    local url="$2"
    shift 2

    while true; do
        if curl -fsS --max-time 5 "$@" "$url" >/dev/null 2>&1; then
            echo "ok   ${name}"
            return 0
        fi
        if (( SECONDS >= deadline )); then
            echo "FAIL ${name} (${url})" >&2
            return 1
        fi
        sleep 2
    done
}

check "caddy admin health" "${caddy_admin}/health"
check "portal /healthz" "${caddy_https}/healthz" -k -H "Host: ${domain_portal}"
check "tapes /healthz" "${caddy_https}/healthz" -k -H "Host: ${domain_tapes}"
check "scheduler /healthz" "${scheduler_base}/healthz"
check "scheduler /readyz" "${scheduler_base}/readyz"
check "hls index.m3u8" "${caddy_https}/hls/index.m3u8" -k -H "Host: ${domain_radio}"

echo "smoke(${env_name}): all checks passed"
