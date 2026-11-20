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
# DEV terminates TLS in Caddy (self-signed) and exposes the admin API.
# PROD sits behind the shared edge Caddy: `Caddyfile.prod` sets `auto_https off`
# and `admin off`, so the checks speak plain HTTP and there is no admin probe.
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

# Non-fatal check: retries briefly, then only warns (never fails the run).
# Used for the HLS stream, which depends on Liquidsoap — currently crashlooping
# with a fallible-source error unrelated to #40 (see #84). The smoke gate must
# still cover the services the deploy actually touches
# (scheduler/portal/tapes/caddy).
check_warn() {
    local name="$1"
    local url="$2"
    shift 2
    local warn_deadline=$((SECONDS + 10))

    while true; do
        if curl -fsS --max-time 5 "$@" "$url" >/dev/null 2>&1; then
            echo "ok   ${name}"
            return 0
        fi
        if (( SECONDS >= warn_deadline )); then
            echo "WARN ${name} (${url}) — non-fatal, see #84" >&2
            return 0
        fi
        sleep 2
    done
}

if [[ "$env_name" == "dev" ]]; then
    caddy_port="${ZUKKA_SMOKE_CADDY_PORT:-12180}"
    caddy_admin="${ZUKKA_SMOKE_CADDY_ADMIN:-http://127.0.0.1:12181}"
    scheduler_base="${ZUKKA_SMOKE_SCHEDULER:-http://127.0.0.1:12110}"

    domain_radio="${ZUKKA_DOMAIN_RADIO:-radio.dev.zukkafabrik.de}"
    domain_portal="${ZUKKA_DOMAIN_PORTAL:-portal.dev.zukkafabrik.de}"
    domain_tapes="${ZUKKA_DOMAIN_TAPES:-tapes.dev.zukkafabrik.de}"

    scheme="https"
    tls_opts=(-k)

    # Caddy exposes no /health on the admin API; its real liveness probe is
    # GET /config/ (200 once the admin listener is up).
    #
    # The app-router is reached with `--resolve`: the URL carries the real
    # domain (so Caddy gets an SNI and routes by Host) while the connection
    # targets the loopback listener. `-k` skips the self-signed DEV cert.
    check "caddy admin /config/" "${caddy_admin}/config/"
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
check_warn "hls index.m3u8" "${scheme}://${domain_radio}:${caddy_port}/hls/index.m3u8" \
    "${tls_opts[@]}" --resolve "${domain_radio}:${caddy_port}:${caddy_addr}"

echo "smoke(${env_name}): all checks passed"