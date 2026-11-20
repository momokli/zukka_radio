//! Zukka Radio portal — thin UI over the scheduler's identity authority.
//!
//! The portal owns no identity state: it renders HTML (axum + askama + htmx),
//! holds the browser session cookie and talks to the scheduler's `/internal/*`
//! API for verification, session write-through and password changes
//! (§4.1/§4.3). There is no JSON API and no migration of its own.

pub mod announcements;
pub mod api_client;
pub mod app;
pub mod auth;
pub mod config;
pub mod control;
pub mod error;
pub mod ffprobe;
pub mod library;
pub mod music_api;
pub mod queues;
pub mod rate_limit;
pub mod rbac;
pub mod schedule;
pub mod shows;
pub mod templates;
pub mod transcode;
pub mod tz;
pub mod upload;
pub mod upload_pipeline;
pub mod visibility;
pub mod waveform;
