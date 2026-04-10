//! Outbound AP activity publishing.
//!
//! `publish_post_note`  — Create(Note) for a new post
//! `update_post_note`   — Update(Note) when title / url / content changes

use crate::{
    ap::{
        signature::{compute_digest, sign_request},
        types::PUBLIC_URI,
    },
    error::AppResult,
    state::AppState,
};
use chrono::Utc;
use markdown::{to_html_with_options, Options};
use reqwest::header::CONTENT_TYPE;
use url::Url;
use uuid::Uuid;

// ── Create ────────────────────────────────────────────────────────────────────

/// Publish a Create(Note) for a newly registered post.
/// Stores the Note ID in `posts.ap_note_id`, then fans out to all followers.
pub async fn publish_post_note(
    state: &AppState,
    post_id: &str,
    title: Option<&str>,
    url: &str,
    content: &str,
) -> AppResult<String> {
    let base = state.config.base_url();
    let note_id = format!("{base}/notes/{}", Uuid::new_v4());
    let now = now_str();
    let note = build_note(&note_id, title, url, content, &now, &now, state);

    let activity = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id":    format!("{base}/activities/create/{}", Uuid::new_v4()),
        "type":  "Create",
        "actor": state.config.actor_url(),
        "object": note,
        "published": now,
        "to": [PUBLIC_URI],
        "cc": [format!("{base}/followers")],
    });

    // Persist before delivery so inReplyTo matching works immediately
    sqlx::query("UPDATE posts SET ap_note_id = $1, updated_at = NOW() WHERE id = $2")
        .bind(&note_id)
        .bind(post_id)
        .execute(&state.db)
        .await?;

    fan_out(state, activity).await;
    Ok(note_id)
}

// ── Update ────────────────────────────────────────────────────────────────────

/// Send an Update(Note) when title, url, or content changes.
pub async fn update_post_note(
    state: &AppState,
    note_id: &str,
    title: Option<&str>,
    url: &str,
    content: &str,
    published_at: &str,
) -> AppResult<()> {
    let base = state.config.base_url();
    let now = now_str();
    let mut note = build_note(note_id, title, url, content, published_at, &now, state);
    note["updated"] = serde_json::Value::String(now.clone());

    let activity = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id":    format!("{base}/activities/update/{}", Uuid::new_v4()),
        "type":  "Update",
        "actor": state.config.actor_url(),
        "object": note,
        "published": now,
        "to": [PUBLIC_URI],
        "cc": [format!("{base}/followers")],
    });

    fan_out(state, activity).await;
    Ok(())
}

// ── Note builder ──────────────────────────────────────────────────────────────

fn build_note(
    note_id: &str,
    title: Option<&str>,
    url: &str,
    content_md: &str,
    published: &str,
    _updated: &str,
    state: &AppState,
) -> serde_json::Value {
    let followers_url = format!("{}/followers", state.config.base_url());
    let content_html = render_note_html(title, url, content_md);

    serde_json::json!({
        "id":           note_id,
        "type":         "Note",
        "attributedTo": state.config.actor_url(),
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

async fn fan_out(state: &AppState, activity: serde_json::Value) {
    let inboxes: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT COALESCE(shared_inbox_url, inbox_url) FROM followers WHERE accepted = TRUE",
    )
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
    tokio::spawn(async move {
        deliver_to_inboxes(&state, &body, &inboxes).await;
    });
}

async fn deliver_to_inboxes(state: &AppState, body: &[u8], inboxes: &[String]) {
    use futures::stream::{self, StreamExt};

    let digest = compute_digest(body);

    stream::iter(inboxes)
        .for_each_concurrent(10, |inbox_url| {
            let state = state.clone();
            let body = body.to_vec();
            let digest = digest.clone();
            let inbox_url = inbox_url.clone();
            async move {
                if let Err(e) = deliver_one(&state, &body, &digest, &inbox_url).await {
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
) -> anyhow::Result<()> {
    let parsed = Url::parse(inbox_url)?;
    let host = parsed.host_str().unwrap_or("").to_string();
    let path = parsed.path().to_string();

    let sig = sign_request(
        "post",
        &path,
        &host,
        Some(body),
        &state.key.private_key,
        &state.config.key_id(),
    )?;

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
