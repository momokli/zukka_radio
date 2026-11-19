//! Zukka Radio scheduler library.
//!
//! Exposes modules so integration tests can reuse the database bootstrap
//! (including the `sqlx` migrations) and exercise the API router directly.

pub mod api;
pub mod db;
pub mod dto;
pub mod error;
pub mod priority;
pub mod rrule;
pub mod store;
pub mod time;
pub mod uuid;
