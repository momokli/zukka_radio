#!/usr/bin/env bash
# End-to-end test for deploy/systemd/zukka-backup against a LOCAL restic repo.
# Run: scripts/zukka-backup-test.sh   (needs restic + sqlite3; no PROD access)
#
# Proves the drill path (backup -> restore -> integrity_check -> RTO), the
# predeploy no-op on an empty host, and two negative cases (empty repo, corrupt
# DB) actually turn the drill red. Used by the CI job `backup` (#43).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/.." && pwd)"
backup_bin="${repo_root}/deploy/systemd/zukka-backup"

for tool in restic sqlite3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "SKIP: $tool not installed" >&2
        exit 0
    fi
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

export RESTIC_REPOSITORY="${work}/repo"
export RESTIC_PASSWORD="test-password"
export ZUKKA_BACKUP_STAGE_ROOT="${work}/stage"
export ZUKKA_BACKUP_DRILL_DIR="${work}/drill"
export ZUKKA_BACKUP_ENV_DIR="${work}/etc"
export ZUKKA_BACKUP_APP_ROOT="${work}/app"
export ZUKKA_BACKUP_DB_SRC="${work}/src/zukka.db"
export ZUKKA_BACKUP_STAGING_SRC="${work}/nonexistent-staging"
mkdir -p "${work}/src" "${work}/etc" "${work}/app/prod"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# A minimal but non-trivial DB so integrity_check + schema count are meaningful.
sqlite3 "$ZUKKA_BACKUP_DB_SRC" \
    "CREATE TABLE casters(id TEXT PRIMARY KEY); INSERT INTO casters VALUES ('c1');"
printf 'ZUKKA_TEST=1\n' > "${work}/etc/prod.env"
printf 'sha=deadbeef\n' > "${work}/app/prod/.deployed"

echo "== 1. backup creates a snapshot"
"$backup_bin" backup
"$backup_bin" snapshots | grep -q "zukka" || fail "no zukka snapshot after backup"

echo "== 2. predeploy snapshot"
"$backup_bin" predeploy 0123456789abcdef0123456789abcdef01234567

echo "== 3. drill is green against a good snapshot"
"$backup_bin" drill

echo "== 4. prune keeps the repo usable"
"$backup_bin" prune
"$backup_bin" drill

echo "== 5. predeploy with no DB is a documented no-op (first deploy)"
mv "$ZUKKA_BACKUP_DB_SRC" "${ZUKKA_BACKUP_DB_SRC}.bak"
"$backup_bin" predeploy 0123456789abcdef0123456789abcdef01234567
mv "${ZUKKA_BACKUP_DB_SRC}.bak" "$ZUKKA_BACKUP_DB_SRC"

echo "== 6. drill is red when the DB inside the snapshot is corrupt"
# Put a corrupt zukka.db into the staging tree and snapshot it as newest.
rm -rf "${work}/stage/corrupt" && mkdir -p "${work}/stage/corrupt/data"
printf 'this is not a sqlite database' > "${work}/stage/corrupt/data/zukka.db"
( cd "${work}/stage/corrupt" && restic backup --host zukka --tag zukka . >/dev/null )
if "$backup_bin" drill >/dev/null 2>&1; then
    fail "drill accepted a corrupt DB"
fi

echo "== 7. drill is red when the repo has no zukka snapshot"
# A freshly initialised repo holds zero snapshots; the drill must refuse it.
export RESTIC_REPOSITORY="${work}/empty-repo"
restic init >/dev/null
export ZUKKA_BACKUP_DRILL_DIR="${work}/drill-empty"
if "$backup_bin" drill >/dev/null 2>&1; then
    fail "drill accepted an empty repo"
fi

echo "backup-test: all checks passed"
