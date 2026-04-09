//! Outbound HTTP fetching with HTTP Signature authentication.
//!
//! All GETs to remote AP servers are signed with the instance actor key.
//! This is required by GoToSocial and recommended for all implementations.
//!
//! SSRF mitigations are applied to every URL before fetching.

use crate::{
    ap::{signature, types::RemoteActor},
    error::{AppError, AppResult},
    state::AppState,
};
use reqwest::{header::ACCEPT, Client};
use rsa::RsaPrivateKey;
use std::{net::IpAddr, str::FromStr, time::Duration};
use url::Url;

const ACCEPT_AP: &str =
    r#"application/activity+json, application/ld+json; profile="https://www.w3.org/ns/activitystreams"; q=0.9"#;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 1024 * 1024; // 1 MiB

/// Build the shared reqwest client (called once at startup).
pub fn build_http_client() -> reqwest::Result<Client> {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!(
            "komentoj/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/example/komentoj)"
        ))
        // Validate each redirect destination against the SSRF blocklist so a
        // public-looking URL cannot bounce us into an internal network address.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 {
                return attempt.error(anyhow::anyhow!("too many redirects"));
            }
            // Extract URL data into owned values before consuming `attempt`.
            let scheme = attempt.url().scheme().to_string();
            let host = attempt.url().host_str().map(str::to_string);
            match scheme.as_str() {
                "http" | "https" => {}
                s => {
                    return attempt
                        .error(anyhow::anyhow!("disallowed redirect scheme: {s}"))
                }
            }
            let Some(host) = host else {
                return attempt.error(anyhow::anyhow!("redirect URL has no host"));
            };
            if host == "localhost" || host == "ip6-localhost" {
                return attempt
                    .error(anyhow::anyhow!("SSRF: disallowed redirect host '{host}'"));
            }
            if let Ok(ip) = host.parse::<IpAddr>() {
                if is_private_ip(ip) {
                    return attempt
                        .error(anyhow::anyhow!("SSRF: disallowed redirect IP '{ip}'"));
                }
            }
            attempt.follow()
        }))
        .build()
}

/// Perform a signed GET to fetch and deserialize a remote AP object.
pub async fn fetch_ap_object<T: serde::de::DeserializeOwned>(
    url: &str,
    client: &Client,
    private_key: &RsaPrivateKey,
    key_id: &str,
) -> AppResult<T> {
    validate_url(url)?;

    let parsed = Url::parse(url)
        .map_err(|e| AppError::BadRequest(format!("invalid URL: {e}")))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("URL has no host".into()))?;
    let path = parsed.path();
    let path_and_query = if let Some(q) = parsed.query() {
        format!("{path}?{q}")
    } else {
        path.to_string()
    };

    let headers = signature::sign_request(
        "get",
        &path_and_query,
        host,
        None,
        private_key,
        key_id,
    )?;

    let response = client
        .get(url)
        .header(ACCEPT, ACCEPT_AP)
        .header("Date", &headers.date)
        .header("Signature", &headers.signature)
        .send()
        .await?;

    let status = response.status();
    if status == reqwest::StatusCode::GONE {
        return Err(AppError::NotFound); // actor deleted (410)
    }
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "remote server returned {status} for {url}"
        )));
    }

    let body = response.bytes().await.map_err(AppError::Http)?;

    if body.len() > MAX_BODY_BYTES {
        return Err(AppError::BadRequest(format!(
            "response from {url} too large ({} bytes)",
            body.len()
        )));
    }

    serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("failed to parse AP response from {url}: {e}")))
}

/// Fetch a remote actor document, using Redis and DB caches if available.
pub async fn fetch_actor(url: &str, state: &AppState) -> AppResult<RemoteActor> {
    let cache_key = format!("actor:{url}");

    // 1. Redis cache (fast path)
    if let Ok(mut conn) = state.redis.get().await {
        use deadpool_redis::redis::AsyncCommands;
        if let Ok(cached) = conn.get::<_, String>(&cache_key).await {
            if let Ok(actor) = serde_json::from_str::<RemoteActor>(&cached) {
                return Ok(actor);
            }
        }
    }

    // 2. Fetch from remote
    let actor: RemoteActor = fetch_ap_object(
        url,
        &state.http,
        &state.key.private_key,
        &state.config.key_id(),
    )
    .await?;

    // 3. Persist to DB
    upsert_actor_cache(state, &actor).await?;

    // 4. Write to Redis
    if let Ok(mut conn) = state.redis.get().await {
        use deadpool_redis::redis::AsyncCommands;
        let json = serde_json::json!({
            "id": actor.id,
            "type": actor.actor_type,
            "preferredUsername": actor.preferred_username,
            "name": actor.name,
            "inbox": actor.inbox,
            "endpoints": actor.endpoints,
            "publicKey": actor.public_key,
            "icon": actor.icon,
            "url": actor.url,
        })
        .to_string();
        let ttl = state.config.redis.actor_cache_ttl;
        let _: Result<(), _> = conn.set_ex(&cache_key, json, ttl).await;
    }

    Ok(actor)
}

/// Fetch a remote Note object (when `object` in a Create activity is a URL string).
pub async fn fetch_note(url: &str, state: &AppState) -> AppResult<serde_json::Value> {
    validate_url(url)?;
    fetch_ap_object(
        url,
        &state.http,
        &state.key.private_key,
        &state.config.key_id(),
    )
    .await
}

/// Store/update actor in the PostgreSQL actor_cache table.
pub async fn upsert_actor_cache(state: &AppState, actor: &RemoteActor) -> AppResult<()> {
    let Some(pk) = &actor.public_key else {
        return Err(AppError::BadRequest("actor has no publicKey".into()));
    };
    let inbox = actor
        .inbox
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("actor has no inbox".into()))?;

    let instance = extract_host(&actor.id)?;
    let preferred_username = actor.preferred_username.as_deref().unwrap_or("");
    let display_name = actor.name.as_deref();
    let avatar_url = actor.avatar_url();
    let profile_url = actor.profile_url();
    let shared_inbox = actor
        .endpoints
        .as_ref()
        .and_then(|e| e.shared_inbox.as_deref());

    let raw = serde_json::json!({
        "id": actor.id,
        "type": actor.actor_type,
        "preferredUsername": actor.preferred_username,
        "name": actor.name,
        "inbox": actor.inbox,
        "publicKey": actor.public_key,
    });

    sqlx::query(
        r#"
        INSERT INTO actor_cache
            (id, preferred_username, display_name, avatar_url, profile_url,
             public_key_id, public_key_pem, inbox_url, shared_inbox_url,
             instance, raw_data, fetched_at, updated_at)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,NOW(),NOW())
        ON CONFLICT (id) DO UPDATE SET
            preferred_username = EXCLUDED.preferred_username,
            display_name       = EXCLUDED.display_name,
            avatar_url         = EXCLUDED.avatar_url,
            profile_url        = EXCLUDED.profile_url,
            public_key_id      = EXCLUDED.public_key_id,
            public_key_pem     = EXCLUDED.public_key_pem,
            inbox_url          = EXCLUDED.inbox_url,
            shared_inbox_url   = EXCLUDED.shared_inbox_url,
            raw_data           = EXCLUDED.raw_data,
            updated_at         = NOW()
        "#,
    )
    .bind(&actor.id)
    .bind(preferred_username)
    .bind(display_name)
    .bind(avatar_url)
    .bind(profile_url)
    .bind(&pk.id)
    .bind(&pk.public_key_pem)
    .bind(inbox)
    .bind(shared_inbox)
    .bind(instance)
    .bind(raw)
    .execute(&state.db)
    .await?;

    Ok(())
}

// ── SSRF guard ────────────────────────────────────────────────────────────────

/// Reject URLs that point to local/private network addresses.
fn validate_url(url: &str) -> AppResult<()> {
    let parsed = Url::parse(url)
        .map_err(|e| AppError::BadRequest(format!("invalid URL '{url}': {e}")))?;

    match parsed.scheme() {
        "https" | "http" => {}
        s => return Err(AppError::BadRequest(format!("disallowed URL scheme: {s}"))),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("URL has no host".into()))?;

    if host == "localhost" || host == "ip6-localhost" {
        return Err(AppError::BadRequest(format!(
            "SSRF: disallowed host '{host}'"
        )));
    }

    // Block IP literals that are private/link-local/loopback
    if let Ok(ip) = IpAddr::from_str(host) {
        if is_private_ip(ip) {
            return Err(AppError::BadRequest(format!(
                "SSRF: disallowed IP address '{ip}'"
            )));
        }
    }

    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                // CGNAT 100.64.0.0/10
                || (u32::from(v4) & 0xFFC0_0000 == 0x6440_0000)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fe80::/10 link-local
                || (v6.segments()[0] & 0xFFC0 == 0xFE80)
                // fc00::/7 ULA
                || (v6.segments()[0] & 0xFE00 == 0xFC00)
        }
    }
}

pub fn extract_host(url: &str) -> AppResult<String> {
    Url::parse(url)
        .map_err(|e| AppError::BadRequest(format!("invalid URL: {e}")))?
        .host_str()
        .ok_or_else(|| AppError::BadRequest("URL has no host".into()))
        .map(str::to_string)
}
