//! Zukka Radio public Tapes API (Issue #24, M3).
//!
//! A strictly read-only, auth-free service over the shared `zukka.db`
//! (`docs/consolidated-architecture.md` §4.5/§4.6, `docs/tapes.md` §3.4). The
//! connection pool opens the database in SQLite `mode=ro`; the crate owns **no**
//! migrations and never writes.
//!
//! Public surface:
//!
//! - `GET /api/v1/healthz` — liveness + DB check (`no-store`)
//! - `GET /api/v1/tapes` — feed (filters, keyset cursor, `next_cursor`/`count`)
//! - `GET /api/v1/tapes/{slug}` — a single tape (§3.3 JSON)
//! - `GET /api/v1/casters` — caster aggregate (`tape_count > 0`)
//!
//! Only `visible = 1 AND status = 'ready'` is public; every other tape
//! (unknown, hidden, pending, processing, failed, deleted) yields the
//! byte-identical `404 not_found` envelope.

pub mod api;
pub mod config;
pub mod cursor;
pub mod db;
pub mod error;
pub mod etag;
pub mod fixtures;
pub mod model;
pub mod store;
pub mod time;
pub mod waveform;
pub mod worker;
