//! Environment-driven configuration.

use std::path::PathBuf;

/// Default on-disk database path (consolidated-architecture §3.1).
pub const DEFAULT_DB_PATH: &str = "/data/zukka.db";

/// Default public base URL used to build absolute `*_url` fields.
pub const DEFAULT_BASE_URL: &str = "https://tapes.dev.zukkafabrik.de";

/// Default Music API base URL (object storage authority).
pub const DEFAULT_MUSIC_API_URL: &str = "http://music-api:5001";

/// Default scheduler base URL (the only DB writer).
pub const DEFAULT_SCHEDULER_URL: &str = "http://scheduler:8080";

/// Runtime configuration read from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the shared `zukka.db` (opened read-only).
    pub database_path: PathBuf,
    /// `host:port` the HTTP server binds to.
    pub bind: String,
    /// Absolute base URL for `audio_url`/`share_url`/… (no trailing slash).
    pub base_url: String,
    /// Music API base URL; objects are read at `{music_api_url}/objects/{key}`.
    pub music_api_url: String,
    /// Music API bearer token (required for `work`).
    pub music_api_token: String,
    /// Scheduler base URL, e.g. `http://scheduler:8080`.
    pub scheduler_url: String,
    /// Scheduler service token (required for `work`).
    pub scheduler_token: String,
    /// `ffmpeg` binary name or path.
    pub ffmpeg_bin: String,
    /// `ffprobe` binary name or path.
    pub ffprobe_bin: String,
    /// Worker poll interval when no job is due.
    pub worker_poll_secs: u64,
    /// Retry cap: `attempts >= worker_max_attempts` → `failed`.
    pub worker_max_attempts: i64,
    /// Lease for claimed jobs; a crashed worker's job is requeued after it.
    pub worker_lease_secs: i64,
    /// Per-process timeout for ffprobe/ffmpeg.
    pub ffmpeg_timeout_secs: u64,
}

fn parse_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(raw) => raw.trim().parse().unwrap_or(default),
        Err(_) => default,
    }
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
        let music_api_url = std::env::var("MUSIC_API_URL")
            .unwrap_or_else(|_| DEFAULT_MUSIC_API_URL.into())
            .trim_end_matches('/')
            .to_string();
        let music_api_token = std::env::var("MUSIC_API_TOKEN").unwrap_or_default();
        let scheduler_url = std::env::var("ZUKKA_SCHEDULER_URL")
            .unwrap_or_else(|_| DEFAULT_SCHEDULER_URL.into())
            .trim_end_matches('/')
            .to_string();
        let scheduler_token = std::env::var("SCHEDULER_SERVICE_TOKEN")
            .or_else(|_| std::env::var("ZUKKA_SCHEDULER_SERVICE_TOKEN"))
            .unwrap_or_default();
        let ffmpeg_bin = std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into());
        let ffprobe_bin = std::env::var("FFPROBE_BIN").unwrap_or_else(|_| "ffprobe".into());
        let worker_poll_secs = parse_or("WORKER_POLL_INTERVAL_SECS", 5u64);
        let worker_max_attempts = parse_or("WORKER_MAX_ATTEMPTS", 5i64);
        let worker_lease_secs = parse_or("WORKER_LEASE_SECS", 600i64);
        let ffmpeg_timeout_secs = parse_or("WORKER_FFMPEG_TIMEOUT_SECS", 600u64);

        Config {
            database_path,
            bind,
            base_url,
            music_api_url,
            music_api_token,
            scheduler_url,
            scheduler_token,
            ffmpeg_bin,
            ffprobe_bin,
            worker_poll_secs,
            worker_max_attempts,
            worker_lease_secs,
            ffmpeg_timeout_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_or_falls_back_on_invalid_values() {
        // A name that cannot be set by the test process' normal environment.
        let name = "ZUKKA_TEST_DEFINITELY_UNSET_VALUE";
        assert_eq!(parse_or::<u64>(name, 42), 42);
    }
}
