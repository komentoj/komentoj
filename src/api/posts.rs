//! Post registration + auto-publish / auto-update API.
//!
//! POST /api/v1/posts/sync
//!
//! Each post has a user-provided `id` (slug) as its sole unique key.
//! `title`, `url`, and `content` (Markdown) are the content fields.
//!
//! Per post on sync:
//!   - New id                                  → publish Create(Note)
//!   - Existing, title/url/content changed     → publish Update(Note)
//!   - Existing, nothing changed               → no-op
//!
//! Posts absent from the list are marked inactive.

use crate::{
    ap::publish::{publish_post_note, update_post_note},
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::State,
    http::{header::AUTHORIZATION, HeaderMap},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Request / Response ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SyncRequest {
    pub posts: Vec<PostInput>,
}

#[derive(Deserialize)]
pub struct PostInput {
    /// Unique identifier — recommend URL slug, e.g. "hello-world-2026".
    pub id: String,
    /// Optional display title shown in the AP Note.
    pub title: Option<String>,
    /// Canonical blog post URL. Shown as the Note's clickable link in Mastodon.
    pub url: String,
    /// Post body in Markdown. Rendered to HTML and included in the AP Note.
    pub content: String,
}

#[derive(Serialize)]
pub struct SyncResponse {
    pub upserted: usize,
    pub published: usize,
    pub updated: usize,
    pub deactivated: usize,
    pub rejected: Vec<RejectedPost>,
}

#[derive(Serialize)]
pub struct RejectedPost {
    pub id: String,
    pub reason: String,
}

// ── Handler ───────────────────────────────────────────────────────────────────

pub async fn sync_posts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SyncRequest>,
) -> AppResult<Json<SyncResponse>> {
    verify_admin_token(&state, &headers)?;

    let mut upserted = 0usize;
    let mut published = 0usize;
    let mut updated = 0usize;
    let mut rejected: Vec<RejectedPost> = Vec::new();
    let mut valid_ids: Vec<String> = Vec::new();

    enum Action {
        Publish {
            id: String,
            title: Option<String>,
            url: String,
            content: String,
        },
        Update {
            id: String,
            title: Option<String>,
            url: String,
            content: String,
            note_id: String,
            registered_at: String,
        },
        NoOp,
    }

    let mut actions: Vec<Action> = Vec::new();

    for post in body.posts {
        if post.id.is_empty() {
            rejected.push(RejectedPost {
                id: post.id,
                reason: "id must not be empty".into(),
            });
            continue;
        }

        // Snapshot existing record before upsert
        let existing = sqlx::query_as::<_, (Option<String>, String, String, Option<String>, DateTime<Utc>)>(
            "SELECT title, url, content, ap_note_id, registered_at FROM posts WHERE id = $1",
        )
        .bind(&post.id)
        .fetch_optional(&state.db)
        .await?;

        // Upsert
        sqlx::query(
            r#"
            INSERT INTO posts (id, title, url, content, active, registered_at, updated_at)
            VALUES ($1, $2, $3, $4, TRUE, NOW(), NOW())
            ON CONFLICT (id) DO UPDATE SET
                title      = EXCLUDED.title,
                url        = EXCLUDED.url,
                content    = EXCLUDED.content,
                active     = TRUE,
                updated_at = NOW()
            "#,
        )
        .bind(&post.id)
        .bind(&post.title)
        .bind(&post.url)
        .bind(&post.content)
        .execute(&state.db)
        .await?;

        upserted += 1;
        valid_ids.push(post.id.clone());

        let action = match existing {
            None => Action::Publish {
                id: post.id,
                title: post.title,
                url: post.url,
                content: post.content,
            },
            Some((prev_title, prev_url, prev_content, note_id_opt, registered_at)) => {
                match note_id_opt {
                    None => Action::Publish {
                        id: post.id,
                        title: post.title,
                        url: post.url,
                        content: post.content,
                    },
                    Some(note_id) => {
                        let changed = prev_title.as_deref() != post.title.as_deref()
                            || prev_url != post.url
                            || prev_content != post.content;
                        if changed {
                            Action::Update {
                                id: post.id,
                                title: post.title,
                                url: post.url,
                                content: post.content,
                                note_id,
                                registered_at: registered_at
                                    .format("%Y-%m-%dT%H:%M:%SZ")
                                    .to_string(),
                            }
                        } else {
                            Action::NoOp
                        }
                    }
                }
            }
        };

        actions.push(action);
    }

    // Deactivate posts absent from this sync.
    // An empty list means "no active posts" — deactivate everything.
    let deactivated = if !valid_ids.is_empty() {
        sqlx::query(
            "UPDATE posts SET active = FALSE, updated_at = NOW() \
             WHERE active = TRUE AND id != ALL($1)",
        )
        .bind(&valid_ids)
        .execute(&state.db)
        .await?
        .rows_affected() as usize
    } else {
        sqlx::query(
            "UPDATE posts SET active = FALSE, updated_at = NOW() WHERE active = TRUE",
        )
        .execute(&state.db)
        .await?
        .rows_affected() as usize
    };

    for action in actions {
        match action {
            Action::Publish { id, title, url, content } => {
                published += 1;
                let s = state.clone();
                tokio::spawn(async move {
                    match publish_post_note(&s, &id, title.as_deref(), &url, &content).await {
                        Ok(note_id) => tracing::info!("published {note_id} for '{id}'"),
                        Err(e) => tracing::error!("publish failed for '{id}': {e}"),
                    }
                });
            }
            Action::Update { id, title, url, content, note_id, registered_at } => {
                updated += 1;
                let s = state.clone();
                tokio::spawn(async move {
                    match update_post_note(&s, &note_id, title.as_deref(), &url, &content, &registered_at).await {
                        Ok(()) => tracing::info!("sent Update(Note) for '{id}'"),
                        Err(e) => tracing::error!("update failed for '{id}': {e}"),
                    }
                });
            }
            Action::NoOp => {}
        }
    }

    Ok(Json(SyncResponse { upserted, published, updated, deactivated, rejected }))
}

// ── Auth ──────────────────────────────────────────────────────────────────────

pub fn verify_admin_token(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let auth = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("missing Authorization header".into()))?;

    let token = auth
        .strip_prefix("Bearer ")
        .ok_or_else(|| AppError::Unauthorized("Authorization must use Bearer scheme".into()))?;

    if !constant_time_eq(token.as_bytes(), state.config.admin.token.as_bytes()) {
        return Err(AppError::Unauthorized("invalid admin token".into()));
    }
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
