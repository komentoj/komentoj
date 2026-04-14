//! Post registration + auto-publish / auto-update API.
//!
//! Endpoints:
//!   POST /api/v1/posts/sync
//!     Legacy single-actor alias — operates on config.instance.username.
//!
//!   POST /api/v1/users/:username/posts/sync
//!     Per-user sync. Authenticated either by the global admin token or by
//!     the user's own `api_token` (users.api_token). The SaaS layer replaces
//!     this with Supabase JWT + user lookup.
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
    state::{AppState, UserKey},
};
use axum::{
    extract::{Path, State},
    Json,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

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

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Legacy: POST /api/v1/posts/sync — operates on the configured owner.
pub async fn sync_posts(
    State(state): State<AppState>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    Json(body): Json<SyncRequest>,
) -> AppResult<Json<SyncResponse>> {
    // Admin token only, for backward compatibility.
    if !constant_time_eq(bearer.token(), &state.config.admin.token) {
        return Err(AppError::Unauthorized("invalid admin token".into()));
    }
    let user = state.load_user_key(state.owner_user_id).await?;
    sync_posts_impl(&state, &user, body).await.map(Json)
}

/// Per-user: POST /api/v1/users/:username/posts/sync — admin token OR the
/// user's own api_token (stored in `users.api_token`).
pub async fn sync_posts_for_user(
    State(state): State<AppState>,
    Path(username): Path<String>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    Json(body): Json<SyncRequest>,
) -> AppResult<Json<SyncResponse>> {
    let user = state.find_user(&username).await?;
    let token = bearer.token();

    // Authenticate: either the global admin token (OSS deployments) or the
    // user's own api_token.
    let admin_ok = constant_time_eq(token, &state.config.admin.token);
    let user_token: Option<String> =
        sqlx::query_scalar("SELECT api_token FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::from)?;
    let user_ok = user_token
        .as_deref()
        .is_some_and(|t| !t.is_empty() && constant_time_eq(token, t));

    if !(admin_ok || user_ok) {
        return Err(AppError::Unauthorized("invalid token for user".into()));
    }

    let user_key = state.load_user_key(user.id).await?;
    sync_posts_impl(&state, &user_key, body).await.map(Json)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

// ── Core sync logic ───────────────────────────────────────────────────────────

async fn sync_posts_impl(
    state: &AppState,
    user: &UserKey,
    body: SyncRequest,
) -> AppResult<SyncResponse> {
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
        let existing = sqlx::query_as::<
            _,
            (
                Option<String>,
                String,
                String,
                Option<String>,
                DateTime<Utc>,
            ),
        >(
            "SELECT title, url, content, ap_note_id, registered_at \
             FROM posts WHERE id = $1 AND user_id = $2",
        )
        .bind(&post.id)
        .bind(user.user_id)
        .fetch_optional(&state.db)
        .await?;

        // Upsert. posts.id is a globally-unique slug PK today — multi-user
        // deployments get cross-user collision rejection via the user_id
        // guard in ON CONFLICT UPDATE.
        sqlx::query(
            r#"
            INSERT INTO posts (id, user_id, title, url, content, active, registered_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, TRUE, NOW(), NOW())
            ON CONFLICT (id) DO UPDATE SET
                title      = EXCLUDED.title,
                url        = EXCLUDED.url,
                content    = EXCLUDED.content,
                active     = TRUE,
                updated_at = NOW()
            WHERE posts.user_id = EXCLUDED.user_id
            "#,
        )
        .bind(&post.id)
        .bind(user.user_id)
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
            Some((_, _, _, None, _)) => Action::Publish {
                id: post.id,
                title: post.title,
                url: post.url,
                content: post.content,
            },
            Some((prev_title, prev_url, prev_content, Some(note_id), registered_at)) => {
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
                        registered_at: registered_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    }
                } else {
                    Action::NoOp
                }
            }
        };

        actions.push(action);
    }

    // Deactivate posts absent from this sync.
    let deactivated = if !valid_ids.is_empty() {
        sqlx::query(
            "UPDATE posts SET active = FALSE, updated_at = NOW() \
             WHERE user_id = $1 AND active = TRUE AND id != ALL($2)",
        )
        .bind(user.user_id)
        .bind(&valid_ids)
        .execute(&state.db)
        .await?
        .rows_affected() as usize
    } else {
        sqlx::query(
            "UPDATE posts SET active = FALSE, updated_at = NOW() \
             WHERE user_id = $1 AND active = TRUE",
        )
        .bind(user.user_id)
        .execute(&state.db)
        .await?
        .rows_affected() as usize
    };

    for action in actions {
        match action {
            Action::Publish {
                id,
                title,
                url,
                content,
            } => {
                published += 1;
                let s = state.clone();
                let user = user.clone();
                tokio::spawn(async move {
                    match publish_post_note(&s, &user, &id, title.as_deref(), &url, &content).await
                    {
                        Ok(note_id) => tracing::info!("published {note_id} for '{id}'"),
                        Err(e) => tracing::error!("publish failed for '{id}': {e}"),
                    }
                });
            }
            Action::Update {
                id,
                title,
                url,
                content,
                note_id,
                registered_at,
            } => {
                updated += 1;
                let s = state.clone();
                let user = user.clone();
                tokio::spawn(async move {
                    match update_post_note(
                        &s,
                        &user,
                        &note_id,
                        title.as_deref(),
                        &url,
                        &content,
                        &registered_at,
                    )
                    .await
                    {
                        Ok(()) => tracing::info!("sent Update(Note) for '{id}'"),
                        Err(e) => tracing::error!("update failed for '{id}': {e}"),
                    }
                });
            }
            Action::NoOp => {}
        }
    }

    Ok(SyncResponse {
        upserted,
        published,
        updated,
        deactivated,
        rejected,
    })
}
