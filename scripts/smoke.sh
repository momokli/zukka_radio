#!/usr/bin/env bash
# Post-deploy smoke test.
#
#   ./scripts/smoke.sh dev
#   ./scripts/smoke.sh prod
#
# Runs on the deploy host: all endpoints are checked through the loopback
# listeners (DEV block 121xx, PROD block 120xx). Every check retries until
# ZUKKA_SMOKE_TIMEOUT seconds (default 30) have elapsed, then the script exits
# non-zero.
#
# Both environments run the stack Caddy with `admin off` and `auto_https off`.
# DEV is plain HTTP behind the host edge Caddy (which terminates TLS and
# proxies to 127.0.0.1:12180); PROD sits behind the shared edge Caddy.
# Liveness is the site-level `/__zu/health` route in both cases — there is no
# admin API to probe.
set -euo pipefail

env_name="${1:-}"
case "$env_name" in
    dev | prod) ;;
    *)
        echo "usage: $0 <dev|prod>" >&2
        exit 2
        ;;
esac

caddy_addr="${ZUKKA_SMOKE_CADDY_ADDR:-127.0.0.1}"

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

if [[ "$env_name" == "dev" ]]; then
    caddy_port="${ZUKKA_SMOKE_CADDY_PORT:-12180}"
    scheduler_base="${ZUKKA_SMOKE_SCHEDULER:-http://127.0.0.1:12110}"

    domain_radio="${ZUKKA_DOMAIN_RADIO:-radio.dev.zukkafabrik.de}"
    domain_portal="${ZUKKA_DOMAIN_PORTAL:-portal.dev.zukkafabrik.de}"
    domain_tapes="${ZUKKA_DOMAIN_TAPES:-tapes.dev.zukkafabrik.de}"

    scheme="http"
    tls_opts=()

    # `admin off` in Caddyfile.dev: the site-level health route is the
    # liveness check (same as PROD).
    check "caddy /__zu/health" "http://${caddy_addr}:${caddy_port}/__zu/health"
else
    caddy_port="${ZUKKA_SMOKE_CADDY_PORT:-12080}"
    scheduler_base="${ZUKKA_SMOKE_SCHEDULER:-http://127.0.0.1:12010}"

    domain_radio="${ZUKKA_DOMAIN_RADIO:-radio.zukkafabrik.de}"
    domain_portal="${ZUKKA_DOMAIN_PORTAL:-portal.zukkafabrik.de}"
    domain_tapes="${ZUKKA_DOMAIN_TAPES:-tapes.zukkafabrik.de}"

    scheme="http"
    tls_opts=()

    # `admin off` in Caddyfile.prod: no admin API probe. The site-level
    # health route is the replacement liveness check.
    check "caddy /__zu/health" "http://${caddy_addr}:${caddy_port}/__zu/health"
fi

# Host routing via --resolve works for both environments; only scheme/TLS
# options differ.
check "portal /healthz" "${scheme}://${domain_portal}:${caddy_port}/healthz" \
    "${tls_opts[@]}" --resolve "${domain_portal}:${caddy_port}:${caddy_addr}"
check "tapes /healthz" "${scheme}://${domain_tapes}:${caddy_port}/healthz" \
    "${tls_opts[@]}" --resolve "${domain_tapes}:${caddy_port}:${caddy_addr}"
check "scheduler /healthz" "${scheduler_base}/healthz"
check "scheduler /readyz" "${scheduler_base}/readyz"
check "hls index.m3u8" "${scheme}://${domain_radio}:${caddy_port}/hls/index.m3u8" \
    "${tls_opts[@]}" --resolve "${domain_radio}:${caddy_port}:${caddy_addr}"

echo "smoke(${env_name}): all checks passed"
