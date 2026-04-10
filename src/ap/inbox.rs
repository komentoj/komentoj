//! ActivityPub inbox handler.
//!
//! POST /inbox
//!
//! Flow:
//! 1. Buffer the full request body (needed for Digest verification).
//! 2. Parse the Signature header → extract keyId → derive actor URL.
//! 3. Look up the actor's public key (DB cache first, then fetch remote).
//! 4. Verify signature with cached key; if it fails, re-fetch and retry once
//!    (handles key rotation, as done by GoToSocial).
//! 5. Return 202 Accepted immediately.
//! 6. Process the activity in a background Tokio task.
//!
//! Activity types handled:
//!   Create(Note/Article)  → store comment
//!   Update(Note)          → update comment content
//!   Delete                → soft-delete comment
//!   Follow                → persist follower, auto-send Accept(Follow)
//!   Undo(Follow)          → remove follower
//!   Announce / Like / etc → ignored (not relevant for a comment system)

use crate::{
    ap::{
        fetch::{extract_host, fetch_actor, fetch_note},
        html,
        signature::{compute_digest, extract_key_id, key_id_to_actor_url, sign_request, verify_request},
        types::{IncomingActivity, Note},
    },
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use std::collections::HashMap;

// ── Handler ───────────────────────────────────────────────────────────────────

pub async fn inbox_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    match handle_inbox(state, headers, body).await {
        Ok(()) => StatusCode::ACCEPTED,
        Err(AppError::Unauthorized(msg)) => {
            tracing::warn!("inbox 401: {msg}");
            StatusCode::UNAUTHORIZED
        }
        Err(AppError::BadRequest(msg)) => {
            tracing::warn!("inbox 400: {msg}");
            StatusCode::BAD_REQUEST
        }
        Err(AppError::NotFound) => StatusCode::NOT_FOUND,
        Err(e) => {
            tracing::error!("inbox 500: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

pub(crate) async fn handle_inbox(
    state: AppState,
    raw_headers: HeaderMap,
    body: Bytes,
) -> AppResult<()> {
    // Normalise all header names to lowercase for signing string reconstruction
    let headers: HashMap<String, String> = raw_headers
        .iter()
        .filter_map(|(k, v)| {
            Some((k.as_str().to_lowercase(), v.to_str().ok()?.to_string()))
        })
        .collect();

    let sig_header = headers
        .get("signature")
        .ok_or_else(|| AppError::Unauthorized("missing Signature header".into()))?;

    let key_id = extract_key_id(sig_header)?;
    let actor_url = key_id_to_actor_url(&key_id);

    // Try verification with cached key; on failure, re-fetch once (key rotation)
    let cached_pem = get_cached_public_key_pem(&state, &actor_url).await;

    let verified = if let Ok(pem) = cached_pem {
        verify_request("post", "/inbox", &headers, &body, &pem).is_ok()
    } else {
        false
    };

    if !verified {
        // Re-fetch the actor and try the fresh key
        tracing::debug!("signature failed with cached key — re-fetching actor {actor_url}");
        let actor = fetch_actor(&actor_url, &state).await?;
        let fresh_pem = actor
            .public_key
            .as_ref()
            .map(|k| k.public_key_pem.clone())
            .ok_or_else(|| AppError::Unauthorized("actor has no publicKey".into()))?;
        verify_request("post", "/inbox", &headers, &body, &fresh_pem)?;
    }

    // Deserialize the activity
    let activity: IncomingActivity = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON: {e}")))?;

    // Enforce that the signed key's actor exactly matches the activity's actor field.
    // Using starts_with would allow "alice" to forge activities for "alice2".
    let payload_actor_id = activity.actor.id().unwrap_or("");
    if !payload_actor_id.is_empty() && payload_actor_id != actor_url {
        return Err(AppError::Unauthorized(format!(
            "actor mismatch: key={actor_url}, claimed={payload_actor_id}"
        )));
    }

    // Transient activities (no id) are valid but we don't need to process them
    let Some(activity_id) = activity.id.clone() else {
        return Ok(());
    };

    // Deduplication — atomically claim the activity ID.
    // ON CONFLICT DO NOTHING means rows_affected == 0 if already seen.
    // Return 200 (not 4xx) so the remote side does not retry.
    let inserted = sqlx::query(
        "INSERT INTO processed_activities (activity_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(&activity_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if inserted == 0 {
        return Ok(()); // duplicate
    }

    // Hand off to background task.
    // On failure we remove the activity from processed_activities so the remote
    // side can retry on its next delivery attempt.
    let state_clone = state.clone();
    let aid = activity_id.clone();
    tokio::spawn(async move {
        if let Err(e) = process_activity(state_clone.clone(), activity).await {
            tracing::error!("activity processing error: {e:#}");
            let _ = sqlx::query(
                "DELETE FROM processed_activities WHERE activity_id = $1",
            )
            .bind(&aid)
            .execute(&state_clone.db)
            .await;
        }
    });

    Ok(())
}

// ── Activity dispatcher ───────────────────────────────────────────────────────

async fn process_activity(
    state: AppState,
    activity: IncomingActivity,
) -> anyhow::Result<()> {
    let actor_id = activity.actor.id().unwrap_or("").to_string();

    match activity.activity_type.as_str() {
        "Create" => handle_create(&state, &activity, &actor_id).await?,
        "Update" => handle_update(&state, &activity, &actor_id).await?,
        "Delete" => handle_delete(&state, &activity, &actor_id).await?,
        "Follow" => handle_follow(&state, &activity, &actor_id).await?,
        "Undo" => handle_undo(&state, &activity, &actor_id).await?,
        t => tracing::debug!("ignoring activity type '{t}'"),
    }

    Ok(())
}

// ── Create(Note) ──────────────────────────────────────────────────────────────

async fn handle_create(
    state: &AppState,
    activity: &IncomingActivity,
    actor_id: &str,
) -> anyhow::Result<()> {
    let Some(object_val) = &activity.object else {
        return Ok(());
    };

    // object can be a URL string or an embedded object — resolve either
    let note_value = resolve_object(object_val, state).await?;

    let note: Note = serde_json::from_value(note_value.clone())
        .map_err(|e| anyhow::anyhow!("failed to parse Note: {e}"))?;

    match note.note_type.as_str() {
        "Note" | "Article" | "Question" => {}
        t => {
            tracing::debug!("Create: ignoring object type '{t}'");
            return Ok(());
        }
    }

    // Verify attributedTo matches the signing actor
    let attributed = note.attributed_to.as_ref().and_then(|a| a.id()).unwrap_or("");
    if !attributed.is_empty() && attributed != actor_id {
        tracing::warn!("Create: attributedTo mismatch (signer={actor_id}, attributed={attributed})");
        return Ok(());
    }

    if !note.is_public() {
        tracing::debug!("Create: skipping non-public note {}", note.id);
        return Ok(());
    }

    // Resolve which registered post this comment belongs to.
    //
    // Priority:
    //  1. inReplyTo matches one of our AP Note IDs  ← main reply flow
    //  2. inReplyTo is a comment we already know    ← reply-to-reply
    //  3. Note content contains a URL matching posts.url ← mention fallback
    let Some(post_id) = resolve_post_id(state, &note).await else {
        tracing::debug!("Create: cannot associate note {} with any registered post", note.id);
        return Ok(());
    };

    // Ensure actor is in our cache (so the FK constraint on comments is satisfied)
    ensure_actor_cached(state, actor_id).await;

    let content_html = html::sanitize_note_html(note.best_content().unwrap_or(""));
    let content_source = note.markdown_source().map(str::to_string);
    let published_at = parse_published(note.published.as_deref()).unwrap_or_else(Utc::now);
    let in_reply_to = note.in_reply_to.as_ref().and_then(|r| r.id()).map(str::to_string);

    let in_reply_to_local = in_reply_to
        .as_deref()
        .map(|id| {
            extract_host(id)
                .map(|h| h == state.config.instance.domain)
                .unwrap_or(false)
        })
        .unwrap_or(false);

    sqlx::query(
        r#"
        INSERT INTO comments
            (id, post_id, actor_id, content_html, content_source,
             published_at, in_reply_to, in_reply_to_local, visibility, raw_data)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'public', $9)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(&note.id)
    .bind(&post_id)
    .bind(actor_id)
    .bind(&content_html)
    .bind(&content_source)
    .bind(published_at)
    .bind(&in_reply_to)
    .bind(in_reply_to_local)
    .bind(&note_value)
    .execute(&state.db)
    .await?;

    tracing::info!("stored comment {} for post '{post_id}'", note.id);
    Ok(())
}

// ── Update(Note) ─────────────────────────────────────────────────────────────

async fn handle_update(
    state: &AppState,
    activity: &IncomingActivity,
    actor_id: &str,
) -> anyhow::Result<()> {
    let Some(object_val) = &activity.object else {
        return Ok(());
    };

    let note_value = resolve_object(object_val, state).await?;
    let note: Note = serde_json::from_value(note_value.clone())
        .map_err(|e| anyhow::anyhow!("failed to parse Note for Update: {e}"))?;

    // Only the owner can update
    let attributed = note.attributed_to.as_ref().and_then(|a| a.id()).unwrap_or("");
    if !attributed.is_empty() && attributed != actor_id {
        return Ok(());
    }

    let content_html = html::sanitize_note_html(note.best_content().unwrap_or(""));
    let content_source = note.markdown_source().map(str::to_string);

    sqlx::query(
        r#"
        UPDATE comments
        SET content_html   = $1,
            content_source = $2,
            raw_data       = $3,
            updated_at     = NOW()
        WHERE id = $4
          AND actor_id = $5
          AND deleted_at IS NULL
        "#,
    )
    .bind(&content_html)
    .bind(&content_source)
    .bind(&note_value)
    .bind(&note.id)
    .bind(actor_id)
    .execute(&state.db)
    .await?;

    tracing::info!("updated comment {}", note.id);
    Ok(())
}

// ── Delete ────────────────────────────────────────────────────────────────────

async fn handle_delete(
    state: &AppState,
    activity: &IncomingActivity,
    actor_id: &str,
) -> anyhow::Result<()> {
    let Some(object_val) = &activity.object else {
        return Ok(());
    };

    // In Delete activities, object is almost always a bare URL string
    let object_id = object_id_from_value(object_val);

    sqlx::query(
        r#"
        UPDATE comments
        SET deleted_at = NOW(), updated_at = NOW()
        WHERE id = $1
          AND actor_id = $2
          AND deleted_at IS NULL
        "#,
    )
    .bind(&object_id)
    .bind(actor_id)
    .execute(&state.db)
    .await?;

    tracing::info!("soft-deleted comment {object_id}");
    Ok(())
}

// ── Follow ────────────────────────────────────────────────────────────────────

async fn handle_follow(
    state: &AppState,
    activity: &IncomingActivity,
    actor_id: &str,
) -> anyhow::Result<()> {
    // Reject Follow activities not directed at our actor — any signed delivery
    // to /inbox would otherwise subscribe the sender to this service's fan-out.
    let our_actor = state.config.actor_url();
    let object_id = activity.object.as_ref().map(object_id_from_value);
    if object_id.as_deref() != Some(our_actor.as_str()) {
        tracing::debug!(
            "Follow: object {:?} does not match our actor, ignoring",
            object_id
        );
        return Ok(());
    }

    let actor = match fetch_actor(actor_id, state).await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Follow: failed to fetch actor {actor_id}: {e}");
            return Ok(());
        }
    };

    let inbox = match actor.preferred_inbox() {
        Some(i) => i.to_string(),
        None => {
            tracing::warn!("Follow: actor {actor_id} has no inbox");
            return Ok(());
        }
    };

    sqlx::query(
        r#"
        INSERT INTO followers (actor_id, inbox_url, accepted)
        VALUES ($1, $2, TRUE)
        ON CONFLICT (actor_id) DO UPDATE SET inbox_url = EXCLUDED.inbox_url, accepted = TRUE
        "#,
    )
    .bind(actor_id)
    .bind(&inbox)
    .execute(&state.db)
    .await?;

    // Send Accept(Follow) back
    let follow_id = activity.id.as_deref().unwrap_or(actor_id);
    send_accept(state, actor_id, follow_id, &inbox).await?;

    tracing::info!("accepted Follow from {actor_id}");
    Ok(())
}

async fn handle_undo(
    state: &AppState,
    activity: &IncomingActivity,
    actor_id: &str,
) -> anyhow::Result<()> {
    let Some(object_val) = &activity.object else {
        return Ok(());
    };

    let object_type = object_val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Check inner actor matches signer (prevent undoing others' actions)
    if let Some(inner_actor) = object_val.get("actor").and_then(|v| v.as_str()) {
        if inner_actor != actor_id {
            tracing::warn!("Undo: inner actor {inner_actor} != signer {actor_id}");
            return Ok(());
        }
    }

    if object_type == "Follow" || object_type.is_empty() {
        sqlx::query("DELETE FROM followers WHERE actor_id = $1")
            .bind(actor_id)
            .execute(&state.db)
            .await?;
        tracing::info!("removed follower {actor_id}");
    }

    Ok(())
}

// ── Accept(Follow) delivery ───────────────────────────────────────────────────

async fn send_accept(
    state: &AppState,
    follower_actor_url: &str,
    follow_activity_id: &str,
    inbox_url: &str,
) -> anyhow::Result<()> {
    let accept = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!(
            "https://{}/activities/accept/{}",
            state.config.instance.domain,
            uuid::Uuid::new_v4()
        ),
        "type": "Accept",
        "actor": state.config.actor_url(),
        "object": {
            "type": "Follow",
            "id": follow_activity_id,
            "actor": follower_actor_url,
            "object": state.config.actor_url(),
        }
    });

    let body = serde_json::to_vec(&accept)?;
    let digest_header = compute_digest(&body);

    let parsed = url::Url::parse(inbox_url)?;
    let host = parsed.host_str().unwrap_or("").to_string();
    let path = parsed.path().to_string();

    let sig_headers = sign_request(
        "post",
        &path,
        &host,
        Some(&body),
        &state.key.private_key,
        &state.config.key_id(),
    )?;

    let response = state
        .http
        .post(inbox_url)
        .header(CONTENT_TYPE, "application/activity+json")
        .header("Date", &sig_headers.date)
        .header("Digest", &digest_header)
        .header("Signature", &sig_headers.signature)
        .body(body)
        .send()
        .await?;

    tracing::debug!("Accept delivery to {inbox_url}: {}", response.status());
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve `object` from an Activity — may be a bare URL string or embedded object.
/// If URL, fetches from the remote server.
async fn resolve_object(value: &Value, state: &AppState) -> anyhow::Result<Value> {
    if let Some(url) = value.as_str() {
        return fetch_note(url, state)
            .await
            .map_err(|e| anyhow::anyhow!("failed to fetch object {url}: {e}"));
    }
    if value.is_object() {
        return Ok(value.clone());
    }
    Err(anyhow::anyhow!("object field is neither string nor object"))
}

/// Extract a string ID from a Value that is either a URL string or an object with `id`.
pub(crate) fn object_id_from_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.get("id")?.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Resolve which registered post (by `posts.id`) a note is commenting on.
///
/// 1. inReplyTo matches one of our AP Note IDs → look up post id
/// 2. inReplyTo is a comment we already have   → inherit its post_id
/// 3. Fallback: URL in note content matches posts.url (mention flow)
async fn resolve_post_id(state: &AppState, note: &Note) -> Option<String> {
    if let Some(reply_to_id) = note.in_reply_to.as_ref().and_then(|r| r.id()) {
        // Step 1: reply to our announcement Note
        let post_id: Option<String> = sqlx::query_scalar::<_, String>(
            "SELECT id FROM posts WHERE ap_note_id = $1 AND active = TRUE",
        )
        .bind(reply_to_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

        if post_id.is_some() {
            return post_id;
        }

        // Step 2: reply to an existing comment
        let post_id: Option<String> = sqlx::query_scalar::<_, String>(
            "SELECT post_id FROM comments WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(reply_to_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

        if post_id.is_some() {
            return post_id;
        }
    }

    // Step 3: URL in note content matches a registered post's optional url field
    extract_post_id_by_url(state, note).await
}

/// Fallback: find a URL in the note content that matches a registered post's `url` field.
/// Returns the post's `id` (not the URL).
async fn extract_post_id_by_url(state: &AppState, note: &Note) -> Option<String> {
    // Collect candidate URLs from AP tag array and HTML content hrefs
    let mut candidates: Vec<String> = Vec::new();

    if let Some(tags) = &note.tag {
        for tag in tags {
            if tag.get("type").and_then(|v| v.as_str()) == Some("Link") {
                if let Some(href) = tag.get("href").and_then(|v| v.as_str()) {
                    candidates.push(href.to_string());
                }
            }
        }
    }
    candidates.extend(extract_hrefs_from_html(note.best_content().unwrap_or("")));

    for url in candidates {
        let post_id: Option<String> = sqlx::query_scalar::<_, String>(
            "SELECT id FROM posts WHERE url = $1 AND active = TRUE",
        )
        .bind(&url)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

        if post_id.is_some() {
            return post_id;
        }
    }

    None
}

/// Extract all href values from anchor tags in an HTML string.
pub(crate) fn extract_hrefs_from_html(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("href=\"") {
        rest = &rest[pos + 6..];
        if let Some(end) = rest.find('"') {
            let url = &rest[..end];
            if url.starts_with("http://") || url.starts_with("https://") {
                urls.push(url.to_string());
            }
        }
    }
    urls
}

pub(crate) fn parse_published(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Get cached public key PEM from the database (without fetching remote).
async fn get_cached_public_key_pem(state: &AppState, actor_url: &str) -> AppResult<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT public_key_pem FROM actor_cache WHERE id = $1",
    )
    .bind(actor_url)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound)
}

/// Ensure the actor exists in our DB cache; fetch if not present.
async fn ensure_actor_cached(state: &AppState, actor_url: &str) {
    let exists: bool = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM actor_cache WHERE id = $1)",
    )
    .bind(actor_url)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);

    if !exists {
        if let Err(e) = fetch_actor(actor_url, state).await {
            tracing::warn!("failed to cache actor {actor_url}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;
    use axum::body::Bytes;
    use chrono::Datelike;
    use std::time::Duration;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    // ── Pure function unit tests ──────────────────────────────────────────────

    #[test]
    fn extract_hrefs_finds_https_links() {
        let html = r#"<p>See <a href="https://blog.example.com/post-1">this post</a> and <a href="https://other.example/foo">that</a>.</p>"#;
        let urls = extract_hrefs_from_html(html);
        assert_eq!(urls, vec![
            "https://blog.example.com/post-1",
            "https://other.example/foo",
        ]);
    }

    #[test]
    fn extract_hrefs_skips_non_http() {
        let html = r#"<a href="mailto:user@example.com">mail</a> <a href="https://good.com/x">ok</a>"#;
        let urls = extract_hrefs_from_html(html);
        assert_eq!(urls, vec!["https://good.com/x"]);
    }

    #[test]
    fn extract_hrefs_empty_when_no_links() {
        assert!(extract_hrefs_from_html("<p>plain text</p>").is_empty());
        assert!(extract_hrefs_from_html("").is_empty());
    }

    #[test]
    fn parse_published_valid_rfc3339() {
        let dt = parse_published(Some("2024-01-15T12:00:00Z"));
        assert!(dt.is_some());
        let dt = dt.unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn parse_published_invalid_returns_none() {
        assert!(parse_published(Some("not a date")).is_none());
        assert!(parse_published(Some("2024-13-01T00:00:00Z")).is_none());
        assert!(parse_published(None).is_none());
    }

    #[test]
    fn object_id_from_string_value() {
        let v = serde_json::json!("https://remote.example/notes/1");
        assert_eq!(object_id_from_value(&v), "https://remote.example/notes/1");
    }

    #[test]
    fn object_id_from_object_with_id_field() {
        let v = serde_json::json!({"id": "https://remote.example/notes/2", "type": "Note"});
        assert_eq!(object_id_from_value(&v), "https://remote.example/notes/2");
    }

    #[test]
    fn object_id_from_unknown_value_is_empty() {
        assert_eq!(object_id_from_value(&serde_json::json!(null)), "");
        assert_eq!(object_id_from_value(&serde_json::json!(42)), "");
    }

    // ── Integration tests (require DATABASE_URL) ──────────────────────────────
    //
    // These use sqlx::test which creates a fresh PostgreSQL database per test,
    // runs all migrations, and tears down afterwards.
    // Set DATABASE_URL=postgres://user:pass@localhost/komentoj_test to run.

    // Helper: convert our HashMap headers to axum's HeaderMap
    fn header_map(h: &std::collections::HashMap<String, String>) -> HeaderMap {
        crate::test_helpers::to_header_map(h)
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_rejects_missing_signature(pool: sqlx::PgPool) {
        let state = make_test_state(pool, TEST_DOMAIN).await;
        let body = Bytes::from(b"{}".to_vec());
        let headers = HeaderMap::new(); // no Signature header

        let result = handle_inbox(state, headers, body).await;
        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_rejects_invalid_signature(pool: sqlx::PgPool) {
        let actor_url = "https://remote.example/users/alice";
        let inbox_url = "https://remote.example/users/alice/inbox";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, actor_url, inbox_url).await;

        let activity = make_follow_activity(
            "https://remote.example/follows/1",
            actor_url,
            &our_actor_url(),
        );
        let body_bytes = serde_json::to_vec(&activity).unwrap();

        // Build headers but with a corrupted signature
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);
        let mut headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);
        headers.insert("signature".into(), "keyId=\"fake\",headers=\"date\",signature=\"AAAA\"".into());

        let result = handle_inbox(state, header_map(&headers), Bytes::from(body_bytes)).await;
        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_rejects_actor_mismatch(pool: sqlx::PgPool) {
        // Key belongs to alice but activity claims bob as actor
        let alice_url = "https://remote.example/users/alice";
        let bob_url = "https://remote.example/users/bob";
        let inbox_url = "https://remote.example/users/alice/inbox";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, alice_url, inbox_url).await;

        // Activity actor is bob, but signed with alice's key
        let activity = make_follow_activity(
            "https://remote.example/follows/1",
            bob_url,        // ← actor field claims bob
            &our_actor_url(),
        );
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", alice_url); // ← signed by alice
        let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);

        let result = handle_inbox(state, header_map(&headers), Bytes::from(body_bytes)).await;
        assert!(matches!(result, Err(AppError::Unauthorized(_))));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_follow_creates_follower_and_sends_accept(pool: sqlx::PgPool) {
        // Start a wiremock server to capture the Accept delivery
        let mock_server = MockServer::start().await;

        let actor_url = "https://remote.example/users/alice";
        let inbox_url = format!("{}/inbox/alice", mock_server.uri());
        let follow_id = "https://remote.example/follows/99";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, actor_url, &inbox_url).await;

        // Capture Accept(Follow) delivery
        Mock::given(method("POST"))
            .and(path("/inbox/alice"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&mock_server)
            .await;

        let activity = make_follow_activity(follow_id, actor_url, &our_actor_url());
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);
        let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);

        handle_inbox(state, header_map(&headers), Bytes::from(body_bytes))
            .await
            .expect("inbox handle failed");

        // Wait for background task to finish
        wait_for(Duration::from_secs(2), || async {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM followers WHERE actor_id = $1")
                .bind(actor_url)
                .fetch_one(&pool)
                .await
                .unwrap_or(0);
            count == 1
        })
        .await;

        // Accept was delivered to the mock inbox
        let received = mock_server.received_requests().await.unwrap();
        assert!(!received.is_empty(), "Accept should have been delivered");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_follow_ignored_when_object_is_wrong(pool: sqlx::PgPool) {
        let actor_url = "https://remote.example/users/alice";
        let inbox_url = "https://remote.example/users/alice/inbox";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, actor_url, inbox_url).await;

        // Follow directed at someone else, not our actor
        let activity = make_follow_activity(
            "https://remote.example/follows/2",
            actor_url,
            "https://other.example/actor", // ← wrong target
        );
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);
        let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);

        handle_inbox(state, header_map(&headers), Bytes::from(body_bytes))
            .await
            .expect("inbox should return Ok (activity is ignored, not rejected)");

        tokio::time::sleep(Duration::from_millis(200)).await;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM followers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no follower should be added for wrong-target Follow");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_create_stores_comment(pool: sqlx::PgPool) {
        let actor_url = "https://remote.example/users/alice";
        let inbox_url = "https://remote.example/users/alice/inbox";
        let post_id = "my-test-post";
        let post_url = "https://blog.example.com/my-test-post";
        let ap_note_id = format!("https://{}/notes/post-1", TEST_DOMAIN);
        let note_id = "https://remote.example/notes/comment-1";
        let activity_id = "https://remote.example/activities/create-1";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, actor_url, inbox_url).await;
        insert_test_post(&pool, post_id, post_url, &ap_note_id).await;

        let note = make_note_json(
            note_id,
            actor_url,
            "<p>Great post!</p>",
            Some(&ap_note_id), // inReplyTo our announcement Note
        );
        let activity = make_create_activity(activity_id, actor_url, note);
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);
        let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);

        handle_inbox(state, header_map(&headers), Bytes::from(body_bytes))
            .await
            .expect("inbox handle failed");

        wait_for(Duration::from_secs(2), || async {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comments WHERE id = $1")
                .bind(note_id)
                .fetch_one(&pool)
                .await
                .unwrap_or(0);
            count == 1
        })
        .await;

        // Verify content was sanitized and stored
        let html: String = sqlx::query_scalar("SELECT content_html FROM comments WHERE id = $1")
            .bind(note_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(html.contains("Great post!"));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_duplicate_activity_is_deduplicated(pool: sqlx::PgPool) {
        let actor_url = "https://remote.example/users/alice";
        let inbox_url = "https://remote.example/users/alice/inbox";
        let post_id = "dedup-post";
        let ap_note_id = format!("https://{}/notes/dedup-note", TEST_DOMAIN);
        let note_id = "https://remote.example/notes/dedup-comment";
        let activity_id = "https://remote.example/activities/dedup-1";

        insert_test_actor(&pool, actor_url, inbox_url).await;
        insert_test_post(&pool, post_id, "https://blog.example.com/dedup", &ap_note_id).await;

        let note = make_note_json(note_id, actor_url, "<p>duplicate</p>", Some(&ap_note_id));
        let activity = make_create_activity(activity_id, actor_url, note);
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);

        // Send the same activity twice
        for _ in 0..2 {
            let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);
            handle_inbox(
                make_test_state(pool.clone(), TEST_DOMAIN).await,
                header_map(&headers),
                Bytes::from(body_bytes.clone()),
            )
            .await
            .expect("inbox handle failed");
        }

        tokio::time::sleep(Duration::from_millis(400)).await;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comments WHERE id = $1")
            .bind(note_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "duplicate activity must produce exactly one comment");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_undo_follow_removes_follower(pool: sqlx::PgPool) {
        let actor_url = "https://remote.example/users/alice";
        let inbox_url = "https://remote.example/users/alice/inbox";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, actor_url, inbox_url).await;

        // Seed an existing follower record
        sqlx::query("INSERT INTO followers (actor_id, inbox_url, accepted) VALUES ($1,$2,TRUE)")
            .bind(actor_url)
            .bind(inbox_url)
            .execute(&pool)
            .await
            .unwrap();

        let activity = make_undo_follow_activity(
            "https://remote.example/undo/1",
            actor_url,
            "https://remote.example/follows/old",
            &our_actor_url(),
        );
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);
        let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);

        handle_inbox(state, header_map(&headers), Bytes::from(body_bytes))
            .await
            .expect("inbox handle failed");

        wait_for(Duration::from_secs(2), || async {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM followers WHERE actor_id = $1")
                .bind(actor_url)
                .fetch_one(&pool)
                .await
                .unwrap_or(1);
            count == 0
        })
        .await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn inbox_delete_soft_deletes_comment(pool: sqlx::PgPool) {
        let actor_url = "https://remote.example/users/alice";
        let inbox_url = "https://remote.example/users/alice/inbox";
        let post_id = "delete-post";
        let ap_note_id = format!("https://{}/notes/delete-note", TEST_DOMAIN);
        let note_id = "https://remote.example/notes/to-delete";

        let state = make_test_state(pool.clone(), TEST_DOMAIN).await;
        insert_test_actor(&pool, actor_url, inbox_url).await;
        insert_test_post(&pool, post_id, "https://blog.example.com/delete", &ap_note_id).await;

        // Seed an existing comment
        sqlx::query(
            r#"INSERT INTO comments
               (id, post_id, actor_id, content_html, published_at, in_reply_to_local, visibility, raw_data)
               VALUES ($1,$2,$3,'<p>old</p>',NOW(),FALSE,'public','{}'"#,
        )
        .bind(note_id)
        .bind(post_id)
        .bind(actor_url)
        .execute(&pool)
        .await
        .unwrap();

        let activity = make_delete_activity(
            "https://remote.example/activities/delete-1",
            actor_url,
            note_id,
        );
        let body_bytes = serde_json::to_vec(&activity).unwrap();
        let key = test_key();
        let key_id = format!("{}#main-key", actor_url);
        let headers = signed_inbox_headers(&body_bytes, key, &key_id, TEST_DOMAIN);

        handle_inbox(state, header_map(&headers), Bytes::from(body_bytes))
            .await
            .expect("inbox handle failed");

        wait_for(Duration::from_secs(2), || async {
            let deleted_at: Option<chrono::DateTime<chrono::Utc>> =
                sqlx::query_scalar("SELECT deleted_at FROM comments WHERE id = $1")
                    .bind(note_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or(None);
            deleted_at.is_some()
        })
        .await;
    }
}
