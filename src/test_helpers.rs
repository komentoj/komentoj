//! Shared test rig — analogous to GoToSocial's `testrig` package.
//!
//! Provides:
//! - A lazy-initialised RSA-2048 test key pair shared across all tests so
//!   key generation (which is slow) only happens once per test run.
//! - AP JSON fixture builders matching real Mastodon / GoToSocial payloads.
//! - `signed_inbox_headers()` — builds a properly-signed Signature header
//!   for POST /inbox requests.
//! - `make_test_state()` — builds a minimal `AppState` suitable for
//!   `sqlx::test` integration tests without a real Redis instance.
//! - `insert_test_actor()` / `insert_test_post()` — seed the test database.

use crate::{
    ap::signature::{compute_digest, sign_request},
    config::{
        AdminConfig, Config, CorsConfig, DatabaseConfig, InstanceConfig, RedisConfig,
        ServerConfig,
    },
    state::{AppState, InstanceKey},
};
use deadpool_redis::{Config as PoolConfig, Runtime};
use reqwest::Client;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{collections::HashMap, sync::{Arc, OnceLock}, time::Duration};

// ── Singleton test RSA key ────────────────────────────────────────────────────

pub struct TestKeyPair {
    pub private_key: rsa::RsaPrivateKey,
    pub public_key: rsa::RsaPublicKey,
    pub public_key_pem: String,
}

static TEST_KEY: OnceLock<TestKeyPair> = OnceLock::new();

/// Returns a lazily-initialised RSA-2048 key pair shared across all tests.
/// Generated once; subsequent calls return the same key.
pub fn test_key() -> &'static TestKeyPair {
    TEST_KEY.get_or_init(|| {
        let mut rng = rand::thread_rng();
        let private_key =
            rsa::RsaPrivateKey::new(&mut rng, 2048).expect("RSA key generation failed");
        let public_key = private_key.to_public_key();
        let public_key_pem = public_key
            .to_public_key_pem(LineEnding::LF)
            .expect("PEM encoding failed");
        TestKeyPair { private_key, public_key, public_key_pem }
    })
}

// ── Test instance constants ───────────────────────────────────────────────────

/// The domain of the *local* komentoj instance under test.
pub const TEST_DOMAIN: &str = "test.example";

pub fn our_actor_url() -> String {
    format!("https://{}/actor", TEST_DOMAIN)
}

pub fn our_key_id() -> String {
    format!("https://{}/actor#main-key", TEST_DOMAIN)
}

// ── AP JSON fixture builders ──────────────────────────────────────────────────

/// Minimal AP `Person` actor document (Mastodon / GoToSocial compatible).
pub fn make_actor_json(actor_url: &str, key_id: &str, inbox_url: &str, pem: &str) -> Value {
    let username = actor_url.split('/').last().unwrap_or("testuser");
    json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1"
        ],
        "id": actor_url,
        "type": "Person",
        "preferredUsername": username,
        "name": "Test User",
        "inbox": inbox_url,
        "publicKey": {
            "id": key_id,
            "owner": actor_url,
            "publicKeyPem": pem
        },
        "url": actor_url
    })
}

/// AP `Note` document, addressed to the public.
pub fn make_note_json(
    note_id: &str,
    actor_url: &str,
    content: &str,
    in_reply_to: Option<&str>,
) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": note_id,
        "type": "Note",
        "attributedTo": actor_url,
        "content": content,
        "inReplyTo": in_reply_to,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "cc": [format!("{}/followers", actor_url)],
        "published": "2024-01-15T12:00:00Z",
        "sensitive": false
    })
}

/// `Create(Note)` activity wrapping an already-built Note value.
pub fn make_create_activity(activity_id: &str, actor_url: &str, note: Value) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": activity_id,
        "type": "Create",
        "actor": actor_url,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "object": note,
        "published": "2024-01-15T12:00:00Z"
    })
}

/// `Follow` activity directed at `object_url`.
pub fn make_follow_activity(activity_id: &str, actor_url: &str, object_url: &str) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": activity_id,
        "type": "Follow",
        "actor": actor_url,
        "object": object_url
    })
}

/// `Delete` activity where `object` is a bare URL string (Mastodon style).
pub fn make_delete_activity(activity_id: &str, actor_url: &str, object_url: &str) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": activity_id,
        "type": "Delete",
        "actor": actor_url,
        "object": object_url
    })
}

/// `Undo(Follow)` activity.
pub fn make_undo_follow_activity(
    activity_id: &str,
    actor_url: &str,
    follow_id: &str,
    object_url: &str,
) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": activity_id,
        "type": "Undo",
        "actor": actor_url,
        "object": {
            "id": follow_id,
            "type": "Follow",
            "actor": actor_url,
            "object": object_url
        }
    })
}

// ── Signed request header builder ────────────────────────────────────────────

/// Build a `HashMap` of lowercase HTTP headers for a signed POST /inbox
/// request, ready to be passed directly to `handle_inbox`.
pub fn signed_inbox_headers(
    body: &[u8],
    key: &TestKeyPair,
    key_id: &str,
    host: &str,
) -> HashMap<String, String> {
    let sig = sign_request("post", "/inbox", host, Some(body), &key.private_key, key_id)
        .expect("sign_request failed");

    let mut h = HashMap::new();
    h.insert("host".into(), host.into());
    h.insert("date".into(), sig.date);
    h.insert("signature".into(), sig.signature);
    h.insert("digest".into(), compute_digest(body));
    h.insert("content-type".into(), "application/activity+json".into());
    h
}

/// Convert a `HashMap<String,String>` of lowercase header names to an
/// `axum::http::HeaderMap` so it can be passed to `handle_inbox`.
pub fn to_header_map(headers: &HashMap<String, String>) -> axum::http::HeaderMap {
    let mut map = axum::http::HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(val)) = (
            axum::http::HeaderName::from_bytes(k.as_bytes()),
            axum::http::HeaderValue::from_str(v),
        ) {
            map.insert(name, val);
        }
    }
    map
}

// ── AppState builder for sqlx::test integration tests ────────────────────────

/// Build a minimal `AppState` for integration tests.
///
/// * The supplied `pool` (from `sqlx::test`) is used as-is; migrations have
///   already been applied by the test macro.
/// * Redis is configured with an unreachable port (65535) so every
///   `pool.get()` fails silently — the code uses `if let Ok` throughout.
/// * The instance RSA key is the shared `test_key()` singleton.
pub async fn make_test_state(pool: PgPool, domain: &str) -> AppState {
    let config = Config {
        server: ServerConfig { host: "127.0.0.1".into(), port: 8080 },
        instance: InstanceConfig {
            domain: domain.to_string(),
            username: "komentoj".into(),
            display_name: "Test".into(),
            summary: "Test instance".into(),
            blog_domains: vec!["blog.example.com".into()],
        },
        database: DatabaseConfig { url: String::new(), max_connections: 5 },
        redis: RedisConfig {
            url: "redis://127.0.0.1:65535".into(),
            actor_cache_ttl: 3600,
        },
        cors: CorsConfig { allowed_origins: vec![] },
        admin: AdminConfig { token: "test-admin-token".into() },
    };

    let redis = PoolConfig::from_url("redis://127.0.0.1:65535")
        .create_pool(Some(Runtime::Tokio1))
        .expect("redis pool creation");

    let key = test_key();
    let instance_key = InstanceKey {
        private_key: Arc::new(key.private_key.clone()),
        public_key: Arc::new(key.public_key.clone()),
        public_key_pem: key.public_key_pem.clone(),
    };

    AppState {
        config: Arc::new(config),
        db: pool,
        redis,
        key: Arc::new(instance_key),
        http: Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("http client"),
    }
}

// ── Database seeders ──────────────────────────────────────────────────────────

/// Insert a remote actor into `actor_cache` using the shared test key pair.
/// This lets the inbox signature verifier find the public key without an
/// outbound HTTP fetch.
pub async fn insert_test_actor(pool: &PgPool, actor_url: &str, inbox_url: &str) {
    let key = test_key();
    let key_id = format!("{}#main-key", actor_url);
    let instance = actor_url
        .split('/')
        .nth(2)
        .unwrap_or("remote.example")
        .to_string();

    sqlx::query(
        r#"
        INSERT INTO actor_cache
            (id, preferred_username, display_name, avatar_url, profile_url,
             public_key_id, public_key_pem, inbox_url, shared_inbox_url,
             instance, raw_data, fetched_at, updated_at)
        VALUES ($1,$2,$3,NULL,$4,$5,$6,$7,NULL,$8,$9,NOW(),NOW())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(actor_url)
    .bind("testuser")
    .bind("Test User")
    .bind(actor_url) // profile_url
    .bind(&key_id)
    .bind(&key.public_key_pem)
    .bind(inbox_url)
    .bind(&instance)
    .bind(json!({
        "id": actor_url,
        "type": "Person",
        "preferredUsername": "testuser",
        "inbox": inbox_url,
        "publicKey": {
            "id": key_id,
            "owner": actor_url,
            "publicKeyPem": key.public_key_pem
        }
    }))
    .execute(pool)
    .await
    .expect("insert_test_actor failed");
}

/// Insert a post into the `posts` table.
pub async fn insert_test_post(pool: &PgPool, post_id: &str, url: &str, ap_note_id: &str) {
    sqlx::query(
        r#"
        INSERT INTO posts (id, title, url, content, ap_note_id, active, registered_at, updated_at)
        VALUES ($1, $2, $3, 'Test content', $4, TRUE, NOW(), NOW())
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(post_id)
    .bind("Test Post")
    .bind(url)
    .bind(ap_note_id)
    .execute(pool)
    .await
    .expect("insert_test_post failed");
}

/// Wait up to `max` for `condition` to return true, polling every 50 ms.
/// Analogous to GoToSocial's `testrig.WaitFor`.
pub async fn wait_for<F, Fut>(max: Duration, condition: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + max;
    loop {
        if condition().await {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_for: condition not satisfied within {max:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
