//! Announcements (Issue #34): template rendering.
//!
//! This module owns the announcement feature. Issue #34 ships the pure
//! render engine ([`render`]); later issues hang their work off the same
//! module: events/dedupe (#35) and delivery (#37) become siblings here, so
//! nothing has to move.

pub mod render;
