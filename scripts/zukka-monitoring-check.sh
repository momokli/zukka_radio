#!/usr/bin/env bash
# Issue #44 — offline validation of the monitoring artifacts (§3.8).
#
# Validates deploy/monitoring/* without any live Prometheus/blackbox/Grafana:
#   - rules.yml       -> promtool check rules
#   - prometheus.yml  -> promtool check config
#   - blackbox.yml    -> blackbox-exporter --config.check
#   - dashboard.json  -> python3 -m json.tool
#   - targets.yml     -> python3 yaml.safe_load
#
# Prometheus/blackbox run from PINNED images (never `latest`); promtool is not
# installed locally. Exit != 0 on the first failing check.
set -euo pipefail

PROM_IMAGE="prom/prometheus:v2.55.1"
BLACKBOX_IMAGE="prom/blackbox-exporter:v0.25.0"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
MON_DIR="${REPO_ROOT}/deploy/monitoring"

if ! command -v docker >/dev/null 2>&1; then
	echo "error: docker is required (promtool/blackbox run from pinned images)" >&2
	exit 1
fi

echo "== promtool check config (prometheus.yml) =="
docker run --rm --entrypoint promtool -v "${MON_DIR}:/cfg:ro" "${PROM_IMAGE}" \
	check config /cfg/prometheus.yml

echo "== promtool check rules (rules.yml) =="
docker run --rm --entrypoint promtool -v "${MON_DIR}:/cfg:ro" "${PROM_IMAGE}" \
	check rules /cfg/rules.yml

echo "== blackbox-exporter --config.check (blackbox.yml) =="
docker run --rm -v "${MON_DIR}:/cfg:ro" "${BLACKBOX_IMAGE}" \
	--config.file=/cfg/blackbox.yml --config.check

echo "== dashboard.json (json.tool) =="
python3 -m json.tool "${MON_DIR}/dashboard.json" >/dev/null

echo "== targets.yml (yaml) =="
python3 -c 'import sys, yaml; yaml.safe_load(open(sys.argv[1], encoding="utf-8"))' \
	"${MON_DIR}/targets.yml"

echo "OK: monitoring artifacts are valid"
