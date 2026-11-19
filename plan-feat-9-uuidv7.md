# Plan: Issue #9 — UUIDv7 Generator (`scheduler/src/uuid.rs`)

Basis: `v2` · Branch `feat/9-uuidv7` · Source of Truth: `docs/consolidated-architecture.md` §3.1
(lokale ID = `TEXT`, kanonischer UUIDv7, 36 Zeichen lowercase, zeit-sortierbar, App-erzeugt).

## Betroffene Dateien
- `scheduler/src/uuid.rs` (neu) — Modul + Unit-Tests.
- `scheduler/src/lib.rs` — `pub mod uuid;` ergänzen.
- `scheduler/Cargo.toml` / Workspace `Cargo.toml` — **keine Änderung** nötig:
  `uuid = "1"` mit `features = ["v7","serde"]` ist bereits Dependency (lock: 1.26.1).

## Öffentliche API
```rust
/// Erzeugt einen kanonischen UUIDv7-String: 36 Zeichen, lowercase, hyphenated.
/// Streng monoton steigend über alle Aufrufe im Prozess.
pub fn new_id() -> String;
```
Rückgabe via `Uuid::now_v7().to_string()` (`Display` liefert bereits kanonisch
8-4-4-4-12 lowercase). Keine weiteren pub-Items.

## Design-Entscheidung (Monotonie)
`Uuid::now_v7()` (uuid 1.26.1) nutzt intern `ContextV7`/`Monotonic`-Zähler und garantiert
**innerhalb eines Prozesses** streng steigende Werte, auch bei mehreren Aufrufen in derselben
Millisekunde (48-bit ms-Präfix, dann `rand_a`/Zähler inkrementiert).
→ Primärpfad: direkt `Uuid::now_v7()`. **Fallback** (falls die Garantie nicht trägt):
eigener Guard — `Mutex<u64>`/`AtomicU64` letzter Zähler + letzter ms-Wert; bei gleichem ms
Zähler inkrementieren, sonst zurücksetzen. Präfix = 48-bit Unix-ms, Version-Nibble `7`,
Variante `10xx` bleiben durch `Uuid::from_bytes`/`new_v7` erhalten.

## User Stories (Reihenfolge)

1. **Modul-Gerüst + Export**
   - Akzeptanz: `scheduler/src/uuid.rs` existiert; `lib.rs` exportiert `pub mod uuid;`;
     `cargo build -p zukka-scheduler` grün.
   - Betroffen: `scheduler/src/uuid.rs`, `scheduler/src/lib.rs` · ~5 LOC.

2. **`new_id()` implementieren (kanonisch + zeit-sortierbar)**
   - Akzeptanz: Rückgabe ist 36 Zeichen, lowercase, parsebar; `version()==7`,
     `get_variant()==RFC4122` (Variante `10xx`); 48-bit ms-Präfix = aktuelle Unix-ms.
   - Betroffen: `scheduler/src/uuid.rs` · ~15 LOC.

3. **Strenge Monotonie absichern**
   - Akzeptanz: ≥ 10_000 Aufrufe in schneller Folge liefern strikt steigende Strings
     (lexikografisch == chronologisch, da kanonisch). Primär über `now_v7`; Fallback-Guard
     nur falls Test rot.
   - Betroffen: `scheduler/src/uuid.rs` · ~10–30 LOC (nur bei Fallback).

4. **Unit-Tests im Modul (`#[cfg(test)]`)**
   - Akzeptanz (alle grün, `cargo test -p zukka-scheduler`):
     - `len == 36` und `s.chars().all(|c| c.is_ascii_lowercase() || c=='-')`.
     - `Uuid::parse_str(&s).is_ok()` und `u.version()==Version::Nil`? → **nein:**
       `u.get_version()==Some(Version::SortRfc)` (Version 7).
     - `for`-Schleife `0..10_000`: `prev < cur` (strikt steigend).
     - Optional: ms-Präfix der ersten 12 Hex-Zeichen == `SystemTime`-ms (±Toleranz).
   - Betroffen: `scheduler/src/uuid.rs` (`mod tests`) · ~40 LOC.

## Implementierungs-Reihenfolge
1 → 2 → 3 (nur bei Bedarf) → 4. Danach `cargo fmt` + `cargo clippy -p zukka-scheduler`
und `cargo test -p zukka-scheduler`.

## Scope-Guard
Nur `scheduler/src/**` (+ ggf. `scheduler/tests/**`). Keine Änderungen an `Cargo.lock`
(außer zwingend), `.github/**`, `compose/**`, `deploy/**`, `env/**`, anderen Crates,
Migrationen. Kein Push auf `v2`/`main`. Commit-Style: `feat(#9): <english>`.

## Rework — 2026-11-18 (Reviewer-Befund: fmt rot)
- `cargo fmt --all` angewandt; `cargo fmt --all -- --check` → EXIT 0.
- `scheduler/tests/uuid_integration.rs`: zwei `#[tokio::test]` ohne async-Nutzung
  (`generated_ids_are_canonical_uuidv7`, `ids_are_strictly_increasing`) auf `#[test]`
  umgestellt.
- `cargo clippy -p zukka-scheduler --all-targets -- -D warnings` → clean.
- `cargo test -p zukka-scheduler` → 31 passed, 0 failed.
- Commit: `style(#9): rustfmt integration test` (momo-clanker[bot], Branch `feat/9-uuidv7`).
