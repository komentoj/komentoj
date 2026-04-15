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

    // ── Per-user URLs (Mastodon-style /users/:username/…) ───────────────────

    /// Actor URL: {base}/users/{username}
    pub fn user_actor_url(&self, username: &str) -> String {
        format!("{}/users/{}", self.base_url(), username)
    }

    /// Key ID: {base}/users/{username}#main-key
    pub fn user_key_id(&self, username: &str) -> String {
        format!("{}/users/{}#main-key", self.base_url(), username)
    }

    /// Per-user inbox URL: {base}/users/{username}/inbox
    pub fn user_inbox_url(&self, username: &str) -> String {
        format!("{}/users/{}/inbox", self.base_url(), username)
    }

    /// Per-user followers URL: {base}/users/{username}/followers
    pub fn user_followers_url(&self, username: &str) -> String {
        format!("{}/users/{}/followers", self.base_url(), username)
    }

    /// Per-user outbox URL
    pub fn user_outbox_url(&self, username: &str) -> String {
        format!("{}/users/{}/outbox", self.base_url(), username)
    }

    /// Per-user Note URL: {base}/users/{username}/notes/{uuid}
    pub fn user_note_url(&self, username: &str, note_uuid: &str) -> String {
        format!("{}/users/{}/notes/{}", self.base_url(), username, note_uuid)
    }

    /// Per-user WebFinger acct URI
    pub fn user_acct(&self, username: &str) -> String {
        format!("acct:{}@{}", username, self.instance.domain)
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
    use figment::{
        providers::{Format, Toml},
        Figment,
    };

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
    fn per_user_urls_format() {
        let cfg = load_toml(VALID_TOML).unwrap();
        assert_eq!(
            cfg.user_actor_url("alice"),
            "https://example.com/users/alice"
        );
        assert_eq!(
            cfg.user_key_id("alice"),
            "https://example.com/users/alice#main-key"
        );
        assert_eq!(
            cfg.user_inbox_url("alice"),
            "https://example.com/users/alice/inbox"
        );
        assert_eq!(
            cfg.user_note_url("alice", "abc"),
            "https://example.com/users/alice/notes/abc"
        );
    }

    #[test]
    fn acct_format() {
        let cfg = load_toml(VALID_TOML).unwrap();
        assert_eq!(cfg.user_acct("komentoj"), "acct:komentoj@example.com");
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
