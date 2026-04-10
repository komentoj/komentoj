//! REST API consumed by the static blog frontend.
//!
//! GET /api/v1/comments?id=<post_id>[&before=<iso8601>][&limit=<n>]

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CommentsQuery {
    /// The post identifier (same `id` used in the sync API).
    pub id: String,
    /// Cursor: return comments published before this ISO 8601 timestamp.
    pub before: Option<String>,
    /// Max top-level comments (default 50, max 100).
    pub limit: Option<i64>,
}

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct CommentsResponse {
    pub post_id: String,
    pub total: i64,
    pub comments: Vec<CommentItem>,
    /// Pass as `before=` in the next request to get older comments.
    pub next_cursor: Option<String>,
}

#[derive(Serialize)]
pub struct CommentItem {
    /// ActivityPub Note URL (stable comment identifier).
    pub id: String,
    pub author: AuthorInfo,
    /// Sanitized HTML — safe to embed directly.
    pub content_html: String,
    /// Original Markdown source if the remote instance provided it (usually null).
    pub content_source: Option<String>,
    pub published_at: DateTime<Utc>,
    /// AP Note ID of the parent comment (may be on a different instance).
    pub in_reply_to: Option<String>,
    /// Link back to the original post on the author's instance.
    pub source_url: String,
    pub instance: String,
    /// Media attachments (images, videos, audio, etc.).
    pub attachments: Vec<Attachment>,
    pub replies: Vec<CommentItem>,
}

#[derive(Serialize)]
pub struct AuthorInfo {
    pub name: String,
    pub username: String,
    pub profile_url: Option<String>,
    pub avatar_url: Option<String>,
    pub instance: String,
}

#[derive(Serialize)]
pub struct Attachment {
    /// Direct URL to the media file.
    pub url: String,
    /// MIME type (e.g. "image/jpeg", "video/mp4").
    pub media_type: Option<String>,
    /// Alt text / description provided by the author.
    pub name: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    /// Blurhash placeholder for images.
    pub blurhash: Option<String>,
}

// ── Handler ───────────────────────────────────────────────────────────────────

pub async fn get_comments(
    State(state): State<AppState>,
    Query(q): Query<CommentsQuery>,
) -> AppResult<Json<CommentsResponse>> {
    let post_id = q.id.trim().to_string();
    if post_id.is_empty() {
        return Err(AppError::BadRequest("id must not be empty".into()));
    }

    // Verify the post exists
    let exists: bool =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM posts WHERE id = $1)")
            .bind(&post_id)
            .fetch_one(&state.db)
            .await?;

    if !exists {
        return Err(AppError::NotFound);
    }

    let limit = q.limit.unwrap_or(50).clamp(1, 100);

    let before: Option<DateTime<Utc>> = q
        .before
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| AppError::BadRequest(format!("invalid 'before' timestamp: {s}")))
        })
        .transpose()?;

    let total: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM comments WHERE post_id = $1 AND deleted_at IS NULL",
    )
    .bind(&post_id)
    .fetch_one(&state.db)
    .await?;

    // Fetch all non-deleted comments in one query; nest in memory
    let rows = sqlx::query_as::<
        _,
        (
            String,                    // c.id
            String,                    // c.content_html
            Option<String>,            // c.content_source
            DateTime<Utc>,             // c.published_at
            Option<String>,            // c.in_reply_to
            Option<serde_json::Value>, // c.raw_data
            String,                    // a.preferred_username
            Option<String>,            // a.display_name
            Option<String>,            // a.profile_url
            Option<String>,            // a.avatar_url
            String,                    // a.instance
        ),
    >(
        r#"
        SELECT
            c.id,
            c.content_html,
            c.content_source,
            c.published_at,
            c.in_reply_to,
            c.raw_data,
            a.preferred_username,
            a.display_name,
            a.profile_url,
            a.avatar_url,
            a.instance
        FROM comments c
        JOIN actor_cache a ON a.id = c.actor_id
        WHERE c.post_id    = $1
          AND c.deleted_at IS NULL
          AND ($2::timestamptz IS NULL OR c.published_at < $2)
        ORDER BY c.published_at ASC
        "#,
    )
    .bind(&post_id)
    .bind(before)
    .fetch_all(&state.db)
    .await?;

    let ids: std::collections::HashSet<String> = rows.iter().map(|r| r.0.clone()).collect();

    let mut flat: Vec<CommentItem> = rows
        .into_iter()
        .map(|r| {
            let raw = r.5.as_ref();

            let source_url = raw
                .and_then(|v| {
                    v.get("url")
                        .or_else(|| v.get("id"))
                        .and_then(|u| u.as_str())
                })
                .unwrap_or(&r.0)
                .to_string();

            let attachments = extract_attachments(raw);

            CommentItem {
                id: r.0,
                author: AuthorInfo {
                    name: r.7.clone().unwrap_or_else(|| r.6.clone()),
                    username: r.6,
                    profile_url: r.8,
                    avatar_url: r.9,
                    instance: r.10.clone(),
                },
                content_html: r.1,
                content_source: r.2,
                published_at: r.3,
                in_reply_to: r.4,
                source_url,
                instance: r.10,
                attachments,
                replies: vec![],
            }
        })
        .collect();

    // Partition top-level vs replies; attach replies one level deep
    let (mut top_level, replies): (Vec<_>, Vec<_>) = flat.drain(..).partition(|c| {
        c.in_reply_to
            .as_ref()
            .map(|p| !ids.contains(p))
            .unwrap_or(true)
    });

    for reply in replies {
        let parent_id = reply.in_reply_to.as_deref().unwrap_or("");
        if let Some(parent) = top_level.iter_mut().find(|c| c.id == parent_id) {
            parent.replies.push(reply);
        } else {
            top_level.push(reply); // deep thread — promote to top-level
        }
    }

    let next_cursor = if top_level.len() > limit as usize {
        top_level
            .get(limit as usize - 1)
            .map(|c| c.published_at.to_rfc3339())
    } else {
        None
    };
    top_level.truncate(limit as usize);

    Ok(Json(CommentsResponse {
        post_id,
        total,
        comments: top_level,
        next_cursor,
    }))
}

/// Extract media attachments from the AP Note's raw_data JSON.
/// Handles both array and single-object forms of the `attachment` field.
fn extract_attachments(raw: Option<&serde_json::Value>) -> Vec<Attachment> {
    let Some(raw) = raw else { return vec![] };

    let items = match raw.get("attachment") {
        Some(serde_json::Value::Array(arr)) => arr.as_slice(),
        Some(obj @ serde_json::Value::Object(_)) => std::slice::from_ref(obj),
        _ => return vec![],
    };

    items
        .iter()
        .filter_map(|item| {
            let url = item.get("url").and_then(|v| v.as_str())?.to_string();
            Some(Attachment {
                url,
                media_type: item
                    .get("mediaType")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                name: item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                width: item.get("width").and_then(|v| v.as_u64()),
                height: item.get("height").and_then(|v| v.as_u64()),
                blurhash: item
                    .get("blurhash")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect()
}
