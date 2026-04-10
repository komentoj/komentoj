use anyhow::{Context, Result};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;

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

fn default_https() -> String {
    "https".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstanceConfig {
    pub domain: String,
    pub username: String,
    pub display_name: String,
    pub summary: String,
    pub blog_domains: Vec<String>,
    /// URL scheme: "https" (default) or "http" (local dev/testing only)
    #[serde(default = "default_https")]
    pub protocol: String,
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
    /// Load configuration from a TOML file, then layer environment variable
    /// overrides on top. Env vars use the prefix `KOMENTOJ_` and double
    /// underscores as path separators, e.g.:
    ///
    ///   KOMENTOJ_SERVER__PORT=9000
    ///   KOMENTOJ_DATABASE__URL=postgres://...
    ///   KOMENTOJ_ADMIN__TOKEN=secret
    pub fn load(path: &str) -> Result<Self> {
        Figment::new()
            .merge(Toml::file(path))
            .merge(Env::prefixed("KOMENTOJ_").split("__"))
            .extract()
            .context("loading configuration")
    }

    /// Base URL: {protocol}://{domain}
    pub fn base_url(&self) -> String {
        format!("{}://{}", self.instance.protocol, self.instance.domain)
    }

    /// Canonical actor URL: {protocol}://{domain}/actor
    pub fn actor_url(&self) -> String {
        format!("{}/actor", self.base_url())
    }

    /// Canonical key ID: {protocol}://{domain}/actor#main-key
    pub fn key_id(&self) -> String {
        format!("{}/actor#main-key", self.base_url())
    }

    /// Inbox URL
    pub fn inbox_url(&self) -> String {
        format!("{}/inbox", self.base_url())
    }

    /// acct: URI for WebFinger
    pub fn acct(&self) -> String {
        format!("acct:{}@{}", self.instance.username, self.instance.domain)
    }

    /// Check whether a URL belongs to one of the configured blog domains
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use figment::{providers::{Format, Toml}, Figment};

    fn load_toml(s: &str) -> Result<Config> {
        Figment::new()
            .merge(Toml::string(s))
            .extract()
            .context("parse test TOML")
    }

    const VALID_TOML: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[instance]
domain = "example.com"
username = "komentoj"
display_name = "Comments"
summary = "A comment service"
blog_domains = ["blog.example.com"]

[database]
url = "postgres://user:pass@localhost/db"
max_connections = 5

[redis]
url = "redis://127.0.0.1:6379"
actor_cache_ttl = 3600

[cors]
allowed_origins = ["https://blog.example.com"]

[admin]
token = "secret-token"
"#;

    #[test]
    fn loads_valid_toml() {
        let cfg = load_toml(VALID_TOML).unwrap();
        assert_eq!(cfg.instance.domain, "example.com");
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.admin.token, "secret-token");
        assert_eq!(cfg.instance.blog_domains, vec!["blog.example.com"]);
    }

    #[test]
    fn actor_url_format() {
        let cfg = load_toml(VALID_TOML).unwrap();
        assert_eq!(cfg.actor_url(), "https://example.com/actor");
        assert_eq!(cfg.key_id(), "https://example.com/actor#main-key");
        assert_eq!(cfg.inbox_url(), "https://example.com/inbox");
    }

    #[test]
    fn acct_format() {
        let cfg = load_toml(VALID_TOML).unwrap();
        assert_eq!(cfg.acct(), "acct:komentoj@example.com");
    }

    #[test]
    fn is_blog_url_matches_configured_domains() {
        let cfg = load_toml(VALID_TOML).unwrap();
        assert!(cfg.is_blog_url("https://blog.example.com/my-post"));
        assert!(cfg.is_blog_url("https://blog.example.com/"));
        assert!(!cfg.is_blog_url("https://other.example.com/post"));
        assert!(!cfg.is_blog_url("not-a-url"));
    }

    #[test]
    fn env_var_overrides_port() {
        std::env::set_var("KOMENTOJ_SERVER__PORT", "9000");
        let cfg = Figment::new()
            .merge(Toml::string(VALID_TOML))
            .merge(figment::providers::Env::prefixed("KOMENTOJ_").split("__"))
            .extract::<Config>()
            .unwrap();
        assert_eq!(cfg.server.port, 9000);
        std::env::remove_var("KOMENTOJ_SERVER__PORT");
    }

    #[test]
    fn env_var_overrides_admin_token() {
        std::env::set_var("KOMENTOJ_ADMIN__TOKEN", "env-override-token");
        let cfg = Figment::new()
            .merge(Toml::string(VALID_TOML))
            .merge(figment::providers::Env::prefixed("KOMENTOJ_").split("__"))
            .extract::<Config>()
            .unwrap();
        assert_eq!(cfg.admin.token, "env-override-token");
        std::env::remove_var("KOMENTOJ_ADMIN__TOKEN");
    }

    #[test]
    fn missing_required_field_fails() {
        let bad_toml = r#"
[server]
host = "127.0.0.1"
# port missing
"#;
        assert!(load_toml(bad_toml).is_err());
    }
}
