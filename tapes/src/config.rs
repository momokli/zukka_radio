//! Environment-driven configuration.

use std::path::PathBuf;

/// Default on-disk database path (consolidated-architecture §3.1).
pub const DEFAULT_DB_PATH: &str = "/data/zukka.db";

/// Default public base URL used to build absolute `*_url` fields.
pub const DEFAULT_BASE_URL: &str = "https://tapes.dev.zukkafabrik.de";

/// Runtime configuration read from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the shared `zukka.db` (opened read-only).
    pub database_path: PathBuf,
    /// `host:port` the HTTP server binds to.
    pub bind: String,
    /// Absolute base URL for `audio_url`/`share_url`/… (no trailing slash).
    pub base_url: String,
}

impl Config {
    /// Reads configuration from the process environment, applying defaults.
    pub fn from_env() -> Self {
        let database_path = std::env::var("DATABASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_DB_PATH));
        let bind = std::env::var("ZUKKA_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let base_url = std::env::var("PUBLIC_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.into())
            .trim_end_matches('/')
            .to_string();
        Config {
            database_path,
            bind,
            base_url,
        }
    }
}
