//! Outbound AP activity publishing.
//!
//! `publish_post_note`  — Create(Note) for a new post
//! `update_post_note`   — Update(Note) when title / url / content changes
//!
//! Both take a `UserKey` so the Note is attributed to the post owner's actor
//! and the outbound POST is signed with that user's key.

use crate::{
    ap::{
        signature::{compute_digest, sign_request},
        types::PUBLIC_URI,
    },
    error::AppResult,
    state::{AppState, UserKey},
};
use chrono::Utc;
use markdown::{to_html_with_options, Options};
use reqwest::header::CONTENT_TYPE;
use rsa::RsaPrivateKey;
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

// ── Create ────────────────────────────────────────────────────────────────────

/// Publish a Create(Note) on behalf of `user`, then fan out to that user's
/// followers. Stores the resulting Note ID in `posts.ap_note_id`.
pub async fn publish_post_note(
    state: &AppState,
    user: &UserKey,
    post_id: &str,
    title: Option<&str>,
    url: &str,
    content: &str,
) -> AppResult<String> {
    let base = state.config.base_url();
    let note_id = state
        .config
        .user_note_url(&user.username, &Uuid::new_v4().to_string());
    let now = now_str();
    let note = build_note(&note_id, &user.username, title, url, content, &now, state);

    let activity = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id":    format!("{base}/activities/create/{}", Uuid::new_v4()),
        "type":  "Create",
        "actor": state.config.user_actor_url(&user.username),
        "object": note,
        "published": now,
        "to": [PUBLIC_URI],
        "cc": [state.config.user_followers_url(&user.username)],
    });

    // Persist before delivery so inReplyTo matching works immediately
    sqlx::query(
        "UPDATE posts SET ap_note_id = $1, updated_at = NOW() WHERE id = $2 AND user_id = $3",
    )
    .bind(&note_id)
    .bind(post_id)
    .bind(user.user_id)
    .execute(&state.db)
    .await?;

    fan_out(state, user, activity).await;
    Ok(note_id)
}

// ── Update ────────────────────────────────────────────────────────────────────

/// Send an Update(Note) when title, url, or content changes.
pub async fn update_post_note(
    state: &AppState,
    user: &UserKey,
    note_id: &str,
    title: Option<&str>,
    url: &str,
    content: &str,
    published_at: &str,
) -> AppResult<()> {
    let base = state.config.base_url();
    let now = now_str();
    let mut note = build_note(
        note_id,
        &user.username,
        title,
        url,
        content,
        published_at,
        state,
    );
    note["updated"] = serde_json::Value::String(now.clone());

    let activity = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id":    format!("{base}/activities/update/{}", Uuid::new_v4()),
        "type":  "Update",
        "actor": state.config.user_actor_url(&user.username),
        "object": note,
        "published": now,
        "to": [PUBLIC_URI],
        "cc": [state.config.user_followers_url(&user.username)],
    });

    fan_out(state, user, activity).await;
    Ok(())
}

// ── Note builder ──────────────────────────────────────────────────────────────

fn build_note(
    note_id: &str,
    username: &str,
    title: Option<&str>,
    url: &str,
    content_md: &str,
    published: &str,
    state: &AppState,
) -> serde_json::Value {
    let followers_url = state.config.user_followers_url(username);
    let content_html = render_note_html(title, url, content_md);

    serde_json::json!({
        "id":           note_id,
        "type":         "Note",
        "attributedTo": state.config.user_actor_url(username),
        "content":      content_html,
        "url":          url,
        "published":    published,
        "to":  [PUBLIC_URI],
        "cc":  [followers_url],
        "source": {
            "content":   content_md,
            "mediaType": "text/markdown",
        },
    })
}

/// Render the AP Note's HTML content from Markdown.
///
/// Structure:
///   <title as h2 link>
///   <rendered markdown body>
pub fn render_note_html(title: Option<&str>, url: &str, content_md: &str) -> String {
    let mut html = String::new();

    // Title line with link back to the blog post
    if let Some(t) = title {
        let t_esc = t
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        html.push_str(&format!(
            r#"<p><strong><a href="{url}">{t_esc}</a></strong></p>"#
        ));
    }

    // Render Markdown body with GFM (tables, strikethrough, autolinks, task lists)
    if !content_md.is_empty() {
        let body = to_html_with_options(content_md, &Options::gfm()).unwrap_or_else(|_| {
            // markdown-rs shouldn't fail on GFM, but fall back to plain text
            let escaped = content_md
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            format!("<p>{escaped}</p>")
        });
        html.push_str(&body);
    }

    html
}

fn now_str() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── Fan-out delivery ──────────────────────────────────────────────────────────

async fn fan_out(state: &AppState, user: &UserKey, activity: serde_json::Value) {
    // shared_inbox_url lives on actor_cache (from the remote actor doc), not
    // on followers; LEFT JOIN lets us prefer the shared inbox when available.
    let inboxes: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT COALESCE(a.shared_inbox_url, f.inbox_url) \
         FROM followers f \
         LEFT JOIN actor_cache a ON a.id = f.actor_id \
         WHERE f.user_id = $1 AND f.accepted = TRUE",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    if inboxes.is_empty() {
        return;
    }

    let body = match serde_json::to_vec(&activity) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("failed to serialize activity: {e}");
            return;
        }
    };
    let state = state.clone();
    let private_key = user.private_key.clone();
    let key_id = state.config.user_key_id(&user.username);
    tokio::spawn(async move {
        deliver_to_inboxes(&state, &body, &inboxes, &private_key, &key_id).await;
    });
}

async fn deliver_to_inboxes(
    state: &AppState,
    body: &[u8],
    inboxes: &[String],
    private_key: &Arc<RsaPrivateKey>,
    key_id: &str,
) {
    use futures::stream::{self, StreamExt};

    let digest = compute_digest(body);

    stream::iter(inboxes)
        .for_each_concurrent(10, |inbox_url| {
            let state = state.clone();
            let body = body.to_vec();
            let digest = digest.clone();
            let inbox_url = inbox_url.clone();
            let private_key = private_key.clone();
            let key_id = key_id.to_string();
            async move {
                if let Err(e) =
                    deliver_one(&state, &body, &digest, &inbox_url, &private_key, &key_id).await
                {
                    tracing::warn!("delivery to {inbox_url} failed: {e}");
                }
            }
        })
        .await;
}

async fn deliver_one(
    state: &AppState,
    body: &[u8],
    digest: &str,
    inbox_url: &str,
    private_key: &Arc<RsaPrivateKey>,
    key_id: &str,
) -> anyhow::Result<()> {
    let parsed = Url::parse(inbox_url)?;
    let host = parsed.host_str().unwrap_or("").to_string();
    let path = parsed.path().to_string();

    let sig = sign_request("post", &path, &host, Some(body), private_key, key_id)?;

    let resp = state
        .http
        .post(inbox_url)
        .header(CONTENT_TYPE, "application/activity+json")
        .header("Date", &sig.date)
        .header("Digest", digest)
        .header("Signature", &sig.signature)
        .body(body.to_vec())
        .send()
        .await?;

    if !resp.status().is_success() && resp.status().as_u16() != 202 {
        anyhow::bail!("inbox returned {}", resp.status());
    }
    Ok(())
}
