//! Portal configuration from the environment (§3.9, portal.md §5).
//!
//! `PORTAL_BASE_URL` must be `https` — the `__Host-` cookie prefix only works
//! over HTTPS, so a misconfiguration fails loudly at startup instead of
//! silently issuing an insecure session.

use anyhow::{Context, bail};

/// Runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Listen address, e.g. `127.0.0.1:8081`.
    pub bind: String,
    /// Public base URL (must be `https`), e.g. `https://portal.example.org`.
    pub base_url: String,
    /// Scheduler base URL, e.g. `http://scheduler:8080`.
    pub scheduler_base_url: String,
    /// Shared portal↔scheduler service token (§4.1).
    pub service_token: String,
    /// Session idle timeout in days (default 7).
    pub session_idle_days: i64,
    /// Session absolute limit in days (default 30).
    pub session_absolute_days: i64,
}

impl Config {
    /// Reads and validates the configuration from the environment.
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = std::env::var("PORTAL_ADDR").unwrap_or_else(|_| "127.0.0.1:8081".into());
        let base_url = std::env::var("PORTAL_BASE_URL").context("PORTAL_BASE_URL is required")?;
        let scheduler_base_url =
            std::env::var("SCHEDULER_BASE_URL").unwrap_or_else(|_| "http://scheduler:8080".into());
        let service_token = std::env::var("SCHEDULER_SERVICE_TOKEN")
            .context("SCHEDULER_SERVICE_TOKEN is required")?;
        let session_idle_days = env_days("SESSION_IDLE_DAYS", 7)?;
        let session_absolute_days = env_days("SESSION_ABSOLUTE_DAYS", 30)?;

        Self::new(
            bind,
            base_url,
            scheduler_base_url,
            service_token,
            session_idle_days,
            session_absolute_days,
        )
    }

    /// Validates and constructs a config (used by `from_env` and tests).
    pub fn new(
        bind: String,
        base_url: String,
        scheduler_base_url: String,
        service_token: String,
        session_idle_days: i64,
        session_absolute_days: i64,
    ) -> anyhow::Result<Self> {
        let trimmed = base_url.trim_end_matches('/').to_string();
        if !trimmed.starts_with("https://") {
            bail!(
                "PORTAL_BASE_URL must start with https:// (the __Host- cookie prefix \
                 requires HTTPS); got {base_url:?}"
            );
        }
        if service_token.is_empty() {
            bail!("SCHEDULER_SERVICE_TOKEN must not be empty");
        }
        Ok(Self {
            bind,
            base_url: trimmed,
            scheduler_base_url: scheduler_base_url.trim_end_matches('/').to_string(),
            service_token,
            session_idle_days,
            session_absolute_days,
        })
    }

    /// The origin used for `Origin`/`Sec-Fetch-Site` CSRF checks.
    pub fn origin(&self) -> &str {
        &self.base_url
    }
}

fn env_days(name: &str, default: i64) -> anyhow::Result<i64> {
    match std::env::var(name) {
        Ok(raw) => raw
            .parse()
            .with_context(|| format!("{name} must be an integer")),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    fn build(base_url: &str) -> anyhow::Result<Config> {
        Config::new(
            "127.0.0.1:8081".into(),
            base_url.into(),
            "http://scheduler:8080".into(),
            "token".into(),
            7,
            30,
        )
    }

    #[test]
    fn https_is_required() {
        assert!(build("https://portal.example.org").is_ok());
        assert!(build("http://portal.example.org").is_err());
        assert!(build("https://portal.example.org/").is_ok());
    }

    #[test]
    fn trailing_slash_is_trimmed() {
        let cfg = build("https://portal.example.org/").unwrap();
        assert_eq!(cfg.origin(), "https://portal.example.org");
    }

    #[test]
    fn empty_service_token_is_rejected() {
        let cfg = Config::new(
            "127.0.0.1:8081".into(),
            "https://portal.example.org".into(),
            "http://scheduler:8080".into(),
            String::new(),
            7,
            30,
        );
        assert!(cfg.is_err());
    }
}
