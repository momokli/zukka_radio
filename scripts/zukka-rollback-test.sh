#!/usr/bin/env bash
# Drill for the zukka-deploy PROD rollback state machine (#47).
# Run: scripts/zukka-rollback-test.sh   (no PROD access, no root, no real Docker)
#
# Proves against a LOCAL fake environment (temp dir + PATH stubs):
#   - `prod --rollback` without `.deployed.prev` fails (T0);
#   - a normal deploy rotates `.deployed` -> `.deployed.prev` exactly once (T1);
#   - `--rollback` deploys `.deployed.prev` without rotating, a second run is a
#     no-op (idempotent) (T2/T3);
#   - a red smoke gate auto-rolls back to the running good stand (T4);
#   - `recover_state` promotes a leftover `.deployed.next` (T5);
#   - a failing pre-deploy backup aborts before any `pull` (fail-closed, T6);
#   - the window gate uses `ZUKKA_SCHEDULER_URL` (T7/T8/T9);
#   - the pre-deploy backup seam honours ZUKKA_BACKUP_BIN and
#     ZUKKA_SKIP_PREDEPLOY_BACKUP (T10/T11).
# Used by the CI job `rollback`.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/.." && pwd)"
deploy_bin="${repo_root}/deploy/systemd/zukka-deploy"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- fake environment (never touches /opt or /etc) -------------------------

app_root="${work}/app"
case "$app_root" in
    "$work"/*) ;;
    *) echo "FAIL: refusing to run outside the temp dir (${app_root})" >&2; exit 1 ;;
esac

export ZUKKA_APP_ROOT="$app_root"
export ZUKKA_PROD_ENV_FILE="${work}/etc/prod.env"

mkdir -p "${app_root}/repo/compose" "${app_root}/repo/scripts" "${work}/etc" "${work}/bin"

state_dir="${app_root}/prod"
docker_log="${work}/docker.log"
backup_log="${work}/backup.log"
out_log="${work}/out.log"
: > "$docker_log"
: > "$backup_log"

# Minimal artifacts the wrapper insists on (existence/executability only).
printf 'services: {}\n' > "${app_root}/repo/compose/prod.yml"
printf 'ZUKKA_SCHEDULER_SERVICE_TOKEN=test-token\n' > "$ZUKKA_PROD_ENV_FILE"

# Fake smoke gate: green unless ZUKKA_TAG equals the injected bad sha.
cat > "${app_root}/repo/scripts/smoke.sh" <<'SMOKE'
#!/usr/bin/env bash
set -eu
tag="${ZUKKA_TAG:-}"
if [[ -n "${ZUKKA_TEST_BAD_SHA:-}" && "$tag" == "$ZUKKA_TEST_BAD_SHA" ]]; then
    echo "smoke: gate failed for tag ${tag}" >&2
    exit 1
fi
echo "smoke: gate ok for tag ${tag}"
SMOKE
chmod +x "${app_root}/repo/scripts/smoke.sh"

# Docker stub: logs argv, exit 0, answers `ps -q icecast` non-empty so the
# wrapper does not turn on `--force` (which would add audio pulls).
cat > "${work}/bin/docker" <<'DOCKER'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${ZUKKA_TEST_DOCKER_LOG:-/dev/null}"
if [[ "$*" == *"ps -q icecast"* ]]; then
    echo "fake-icecast-container-id"
fi
exit 0
DOCKER
chmod +x "${work}/bin/docker"

# Backup stubs: one logs+exits 0, one logs+exits 1 (fail-closed proof).
cat > "${work}/bin/zukka-backup-ok" <<'BK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${ZUKKA_TEST_BACKUP_LOG:-/dev/null}"
exit 0
BK
cat > "${work}/bin/zukka-backup-fail" <<'BK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${ZUKKA_TEST_BACKUP_LOG:-/dev/null}"
exit 1
BK
chmod +x "${work}/bin/zukka-backup-ok" "${work}/bin/zukka-backup-fail"

export PATH="${work}/bin:${PATH}"
export ZUKKA_TEST_DOCKER_LOG="$docker_log"
export ZUKKA_TEST_BACKUP_LOG="$backup_log"
export ZUKKA_BACKUP_BIN="${work}/bin/zukka-backup-ok"
export ZUKKA_TEST_BAD_SHA="ffffffff"

# --- helpers ---------------------------------------------------------------

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

sha_of() {
    sed -n 's/^sha=//p' "$1" 2>/dev/null | head -n 1
}

seed() { # seed <file> <sha>
    printf 'sha=%s\nimage_tag=%s\ntimestamp=2026-01-01T00:00:00Z\nref=refs/heads/v2\n' \
        "$2" "$2" > "$1"
}

reset_state() {
    rm -f "${state_dir}/.deployed" "${state_dir}/.deployed.prev" "${state_dir}/.deployed.next"
}

# Run the wrapper, capture combined output, record the exit code in $last_rc.
run() {
    last_rc=0
    "$deploy_bin" "$@" >"$out_log" 2>&1 || last_rc=$?
}

assert_rc() { # assert_rc <code> <msg>
    [[ "$last_rc" -eq "$1" ]] || fail "$2: expected rc=$1, got $last_rc (last: $(tail -n 1 "$out_log"))"
}

assert_rc_ne0() {
    [[ "$last_rc" -ne 0 ]] || fail "$1: expected rc != 0, got 0"
}

assert_sha() { # assert_sha <file> <sha> <msg>
    [[ -f "$1" ]] || fail "$3: $1 missing"
    [[ "$(sha_of "$1")" == "$2" ]] || fail "$3: expected sha=$2, got $(sha_of "$1")"
}

assert_log() {
    grep -q "$1" "$out_log" || fail "expected log marker '$1' in output"
}

A="aaaaaaaa"
B="bbbbbbbb"
C="cccccccc"
Z="dddddddd"

echo "== T0: rollback without .deployed.prev fails"
reset_state
run prod --rollback
assert_rc_ne0 "T0"
assert_log "deployed.prev to roll back to"

echo "== T1: green deploy rotates .deployed -> .deployed.prev exactly once"
reset_state
mkdir -p "$state_dir"
seed "${state_dir}/.deployed" "$A"
seed "${state_dir}/.deployed.prev" "$Z"
run prod --tag "$B"
assert_rc 0 "T1"
assert_sha "${state_dir}/.deployed" "$B" "T1 deployed"
assert_sha "${state_dir}/.deployed.prev" "$A" "T1 prev"

echo "== T2: rollback deploys .deployed.prev without rotating"
run prod --rollback
assert_rc 0 "T2"
assert_sha "${state_dir}/.deployed" "$A" "T2 deployed"
assert_sha "${state_dir}/.deployed.prev" "$A" "T2 prev stays last-known-good"

echo "== T3: second rollback is a no-op and calls no Docker"
lines_before="$(wc -l < "$docker_log")"
run prod --rollback
assert_rc 0 "T3"
assert_log "no-op"
lines_after="$(wc -l < "$docker_log")"
[[ "$lines_before" -eq "$lines_after" ]] \
    || fail "T3: no-op rollback touched Docker (${lines_before} -> ${lines_after})"

echo "== T4: red smoke auto-rolls back to the running good stand"
reset_state
mkdir -p "$state_dir"
seed "${state_dir}/.deployed" "$A"
seed "${state_dir}/.deployed.prev" "$Z"
export ZUKKA_TEST_BAD_SHA="$B"
run prod --tag "$B"
assert_rc_ne0 "T4"
assert_log "automatic rollback to ${A}"
assert_sha "${state_dir}/.deployed" "$A" "T4 deployed"
assert_sha "${state_dir}/.deployed.prev" "$Z" "T4 prev untouched"
export ZUKKA_TEST_BAD_SHA="ffffffff"

echo "== T5: recover_state promotes a leftover .deployed.next"
reset_state
mkdir -p "$state_dir"
seed "${state_dir}/.deployed.next" "$C"
run prod --rollback
assert_log "recover deploy state: promoting"
assert_sha "${state_dir}/.deployed" "$C" "T5 promoted"
[[ ! -f "${state_dir}/.deployed.next" ]] || fail "T5: .deployed.next not consumed"
assert_rc_ne0 "T5 (no .prev)"

echo "== T6: failing pre-deploy backup aborts before any pull (fail-closed)"
reset_state
mkdir -p "$state_dir"
seed "${state_dir}/.deployed" "$A"
export ZUKKA_BACKUP_BIN="${work}/bin/zukka-backup-fail"
: > "$docker_log"
run prod --tag "$B"
assert_rc_ne0 "T6"
grep -q "pull" "$docker_log" && fail "T6: Docker pull ran despite failing backup"
export ZUKKA_BACKUP_BIN="${work}/bin/zukka-backup-ok"

if command -v curl >/dev/null 2>&1; then
    echo "== T7: --check-window fails closed when the scheduler is unreachable"
    printf 'ZUKKA_SCHEDULER_SERVICE_TOKEN=test-token\n' > "$ZUKKA_PROD_ENV_FILE"
    export ZUKKA_SCHEDULER_URL="http://127.0.0.1:1"
    run prod --check-window
    assert_rc 3 "T7"
    unset ZUKKA_SCHEDULER_URL

    echo "== T8: --check-window fails closed without a service token"
    printf 'ZUKKA_TEST=1\n' > "$ZUKKA_PROD_ENV_FILE"
    run prod --check-window
    assert_rc 3 "T8"

    echo "== T9: --check-window --force short-circuits the gate"
    run prod --check-window --force
    assert_rc 0 "T9"
else
    echo "SKIP: curl not installed; skipping T7-T9 (window gate)" >&2
fi

echo "== T10: pre-deploy backup seam calls the configured binary"
reset_state
mkdir -p "$state_dir"
seed "${state_dir}/.deployed" "$A"
: > "$backup_log"
run prod --tag "$B"
assert_rc 0 "T10"
grep -q "predeploy ${B}" "$backup_log" || fail "T10: backup stub not called with predeploy ${B}"

echo "== T11: ZUKKA_SKIP_PREDEPLOY_BACKUP=1 warns and skips the backup"
reset_state
mkdir -p "$state_dir"
seed "${state_dir}/.deployed" "$A"
: > "$backup_log"
export ZUKKA_SKIP_PREDEPLOY_BACKUP=1
run prod --tag "$B"
assert_rc 0 "T11"
assert_log "WARNING: ZUKKA_SKIP_PREDEPLOY_BACKUP=1"
[[ ! -s "$backup_log" ]] || fail "T11: backup stub was called despite skip flag"
unset ZUKKA_SKIP_PREDEPLOY_BACKUP

echo "rollback-test: all checks passed"
