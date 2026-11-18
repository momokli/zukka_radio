# Progress: Issue #7 — SQLite-Schema (M1 Scheduler Core)

- Repo: momokli/zukka_radio
- Basis: `v2` (3158723)
- Branch: `feat/7-sqlite-schema`
- PR-Ziel: `v2`
- Deliverable: `scheduler/migrations/0001_init.sql` (17 Tabellen, STRICT, UUIDv7 TEXT, WAL-Setup im App-Code), `PRAGMA foreign_key_check` leer, Seed für roles + sources.

## Akzeptanz (Issue #7)
- Migration erzeugt alle 17 Tabellen.
- `PRAGMA foreign_key_check` liefert leer.
- Docs: `docs/consolidated-architecture.md` §3 ist Source of Truth.

## Ist-Stand (Triage)
- Keine `scheduler/migrations/` vorhanden.
- `scheduler/src/main.rs` ist Skelett.
- Kein PR zu #7. → Bauen.
- Baseline (`cargo check -p zukka-scheduler --locked` auf planet, Branch `feat/7-sqlite-schema` @ 3158723): OK, `Finished dev profile ... in 10.97s`, Exit 0.

## Stages
- [x] plan
- [x] setup
- [x] develop
- [x] verify
- [x] test
- [ ] pr
- [ ] review

## Develop (done)
Dateien:
- `scheduler/migrations/0001_init.sql` (17 Tabellen, STRICT, UUIDv7 TEXT, Indizes, Trigger, Seed)
- `scheduler/src/db.rs` (`connect`: WAL, foreign_keys, busy_timeout 5s, synchronous NORMAL + `sqlx::migrate!`)
- `scheduler/src/lib.rs` (`pub mod db;`)
- `scheduler/src/main.rs` (DB-Init aus `ZUKKA_DB`, Default `/data/zukka.db`)
- `scheduler/tests/migration_smoke.rs` (Integrationstest)
- `Cargo.toml`/`scheduler/Cargo.toml`: `tempfile` dev-dependency

## Verify/Test
- `cargo fmt --check`: OK
- `cargo clippy -p zukka-scheduler --all-targets -- -D warnings`: OK (keine Warnungen)
- `cargo test -p zukka-scheduler`: 5 passed; 0 failed
  - migration_creates_17_user_tables ok (count == 17)
  - foreign_key_check_is_empty_and_integrity_ok ok (leer / integrity_check=ok)
  - seed_roles_and_sources_present ok (3 roles, 6 sources)
  - smoke_insert_chain ok (FK-Verletzung abgelehnt)
  - event_log_is_append_only ok (UPDATE/DELETE abgelehnt)

Commit: `feat: SQLite schema migration (issue #7)` — siehe Log.
Pushed: `origin feat/7-sqlite-schema`.
