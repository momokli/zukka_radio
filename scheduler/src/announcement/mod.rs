//! Announcements (Issue #34 + #35): templates, events, retry and delivery.
//!
//! This module owns the announcement feature:
//!
//! - [`render`] — pure template render engine (#34).
//! - [`events`] — pure dedupe-key and payload builders (#35).
//! - [`retry`] — pure retry/backoff policy (#35).
//! - [`delivery`] — the sender seam #37 implements (#35, definition only).

pub mod delivery;
pub mod events;
pub mod render;
pub mod retry;
