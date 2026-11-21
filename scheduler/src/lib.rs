//! Zukka Radio scheduler library.
//!
//! Exposes modules so integration tests can reuse the database bootstrap
//! (including the `sqlx` migrations) and exercise the API router directly.

pub mod announcement;
pub mod api;
pub mod db;
pub mod dto;
pub mod error;
pub mod liquidsoap;
pub mod metrics;
pub mod migrate;
pub mod priority;
pub mod rate_limit;
pub mod recovery;
pub mod rrule;
pub mod store;
pub mod time;
pub mod uuid;
