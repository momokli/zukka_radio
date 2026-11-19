//! Zukka Radio portal — thin UI over the scheduler's identity authority.
//!
//! The portal owns no identity state: it renders HTML (axum + askama + htmx),
//! holds the browser session cookie and talks to the scheduler's `/internal/*`
//! API for verification, session write-through and password changes
//! (§4.1/§4.3). There is no JSON API and no migration of its own.

pub mod api_client;
pub mod app;
pub mod auth;
pub mod config;
pub mod error;
pub mod rate_limit;
pub mod rbac;
pub mod templates;
