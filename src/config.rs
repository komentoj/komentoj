use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub instance: InstanceConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub cors: CorsConfig,
    pub admin: AdminConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstanceConfig {
    pub domain: String,
    pub username: String,
    pub display_name: String,
    pub summary: String,
    pub blog_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    pub url: String,
    pub actor_cache_ttl: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminConfig {
    pub token: String,
}

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("reading config file: {path}"))?;
        toml::from_str(&contents).context("parsing config TOML")
    }

    /// Canonical actor URL: https://{domain}/actor
    pub fn actor_url(&self) -> String {
        format!("https://{}/actor", self.instance.domain)
    }

    /// Canonical key ID: https://{domain}/actor#main-key
    pub fn key_id(&self) -> String {
        format!("https://{}/actor#main-key", self.instance.domain)
    }

    /// Inbox URL
    pub fn inbox_url(&self) -> String {
        format!("https://{}/inbox", self.instance.domain)
    }

    /// acct: URI for WebFinger
    pub fn acct(&self) -> String {
        format!("acct:{}@{}", self.instance.username, self.instance.domain)
    }

    /// Check whether a URL belongs to one of the configured blog domains
    pub fn is_blog_url(&self, url: &str) -> bool {
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        let Some(host) = parsed.host_str() else {
            return false;
        };
        self.instance
            .blog_domains
            .iter()
            .any(|d| d.as_str() == host)
    }
}
