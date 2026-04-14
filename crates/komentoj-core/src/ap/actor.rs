//! Actor and WebFinger endpoints.
//!
//! Per-user routes (canonical):
//!   GET /.well-known/webfinger?resource=acct:{username}@{domain}
//!   GET /users/{username}
//!   GET /users/{username}/outbox
//!   GET /users/{username}/followers
//!   GET /users/{username}/following
//!   GET /users/{username}/notes/{note_uuid}
//!
//! Legacy single-actor aliases (resolve to `config.instance.username`):
//!   GET /actor        → same as /users/{owner}
//!   GET /outbox       → same as /users/{owner}/outbox
//!   GET /followers    → same as /users/{owner}/followers
//!   GET /following    → same as /users/{owner}/following
//!   GET /notes/:id    → same as /users/{owner}/notes/:id

use crate::{
    ap::types::{
        actor_context, ActorDocument, ActorEndpointsOut, PublicKeyObject, WebFingerLink,
        WebFingerResponse, PUBLIC_URI,
    },
    error::{AppError, AppResult},
    state::{AppState, LocalUser},
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

// ── WebFinger ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct WebFingerQuery {
    pub resource: Option<String>,
}

pub async fn webfinger_handler(
    State(state): State<AppState>,
    Query(query): Query<WebFingerQuery>,
) -> AppResult<Response> {
    let resource = query
        .resource
        .ok_or_else(|| AppError::BadRequest("missing resource parameter".into()))?;

    // Accept "acct:username@domain" or the bare user actor URL.
    // Also accept the legacy /actor URL → owner user.
    let username = parse_webfinger_resource(&resource, &state.config.instance.domain)
        .or_else(|| {
            if resource == state.config.actor_url() {
                Some(state.owner_key.username.clone())
            } else {
                None
            }
        })
        .ok_or(AppError::NotFound)?;

    let user = state.find_user(&username).await?;
    let actor = state.config.user_actor_url(&user.username);
    let acct = state.config.user_acct(&user.username);

    let jrd = WebFingerResponse {
        subject: acct,
        aliases: vec![actor.clone()],
        links: vec![
            WebFingerLink {
                rel: "http://webfinger.net/rel/profile-page".into(),
                link_type: Some("text/html".into()),
                href: Some(actor.clone()),
            },
            WebFingerLink {
                rel: "self".into(),
                link_type: Some("application/activity+json".into()),
                href: Some(actor),
            },
        ],
    };

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/jrd+json")],
        Json(jrd),
    )
        .into_response())
}

/// Extract the username from a WebFinger resource param.
/// Accepts: `acct:user@domain`, `https://domain/users/user`.
fn parse_webfinger_resource(resource: &str, expected_domain: &str) -> Option<String> {
    if let Some(acct) = resource.strip_prefix("acct:") {
        let (user, domain) = acct.split_once('@')?;
        if !domain.eq_ignore_ascii_case(expected_domain) {
            return None;
        }
        return Some(user.to_string());
    }
    if let Ok(url) = url::Url::parse(resource) {
        if !url
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(expected_domain))
        {
            return None;
        }
        if let Some(rest) = url.path().strip_prefix("/users/") {
            let user = rest.split('/').next()?;
            if !user.is_empty() {
                return Some(user.to_string());
            }
        }
    }
    None
}

// ── Actor ─────────────────────────────────────────────────────────────────────

pub async fn user_actor_handler(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let user = state.find_user(&username).await?;
    serve_actor_document(&state, &user, &headers).await
}

/// Legacy alias: `/actor` → the configured owner's actor document.
pub async fn actor_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let username = state.owner_key.username.clone();
    let user = state.find_user(&username).await?;
    serve_actor_document(&state, &user, &headers).await
}

async fn serve_actor_document(
    state: &AppState,
    user: &LocalUser,
    headers: &HeaderMap,
) -> AppResult<Response> {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html");

    if !wants_ap(accept) {
        return Ok((
            StatusCode::SEE_OTHER,
            [(header::LOCATION, state.config.base_url())],
        )
            .into_response());
    }

    let actor_url = state.config.user_actor_url(&user.username);
    let key_id = state.config.user_key_id(&user.username);
    let inbox = state.config.user_inbox_url(&user.username);

    let doc = ActorDocument {
        context: actor_context(),
        id: actor_url.clone(),
        actor_type: "Service",
        preferred_username: user.username.clone(),
        name: if user.display_name.is_empty() {
            user.username.clone()
        } else {
            user.display_name.clone()
        },
        summary: user.summary.clone(),
        url: actor_url.clone(),
        inbox: inbox.clone(),
        outbox: state.config.user_outbox_url(&user.username),
        followers: state.config.user_followers_url(&user.username),
        following: format!("{actor_url}/following"),
        endpoints: ActorEndpointsOut {
            shared_inbox: inbox,
        },
        public_key: PublicKeyObject {
            id: key_id,
            owner: Some(actor_url),
            public_key_pem: user.public_key_pem.clone(),
        },
        manually_approves_followers: false,
        discoverable: true,
        published: "2024-01-01T00:00:00Z".into(),
    };

    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            r#"application/activity+json; charset=utf-8"#,
        )],
        Json(doc),
    )
        .into_response())
}

// ── Outbox / following (per-user) ────────────────────────────────────────────

pub async fn user_outbox_handler(
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    state.find_user(&username).await?;
    let base = state.config.user_actor_url(&username);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}/outbox"),
            "type": "OrderedCollection",
            "totalItems": 0,
            "first": format!("{base}/outbox?page=1"),
        })),
    )
        .into_response())
}

pub async fn outbox_handler(State(state): State<AppState>) -> impl IntoResponse {
    let base = state.config.base_url();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}/outbox"),
            "type": "OrderedCollection",
            "totalItems": 0,
            "first": format!("{base}/outbox?page=1"),
        })),
    )
}

pub async fn user_following_handler(
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    state.find_user(&username).await?;
    let base = state.config.user_actor_url(&username);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}/following"),
            "type": "OrderedCollection",
            "totalItems": 0,
        })),
    )
        .into_response())
}

pub async fn following_handler(State(state): State<AppState>) -> impl IntoResponse {
    let base = state.config.base_url();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}/following"),
            "type": "OrderedCollection",
            "totalItems": 0,
        })),
    )
}

// ── Followers (per-user, count only) ─────────────────────────────────────────

pub async fn user_followers_handler(
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    let user = state.find_user(&username).await?;
    let base = state.config.user_actor_url(&username);
    let count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM followers WHERE user_id = $1 AND accepted = TRUE",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}/followers"),
            "type": "OrderedCollection",
            "totalItems": count,
        })),
    )
        .into_response())
}

pub async fn followers_handler(State(state): State<AppState>) -> impl IntoResponse {
    let base = state.config.base_url();
    let count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM followers WHERE user_id = $1 AND accepted = TRUE",
    )
    .bind(state.owner_user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}/followers"),
            "type": "OrderedCollection",
            "totalItems": count,
        })),
    )
}

// ── Note fetch endpoint ───────────────────────────────────────────────────────

/// GET /users/:username/notes/:note_uuid
pub async fn user_note_handler(
    State(state): State<AppState>,
    Path((username, note_uuid)): Path<(String, String)>,
) -> AppResult<Response> {
    let user = state.find_user(&username).await?;
    let note_id = state.config.user_note_url(&username, &note_uuid);
    serve_note(&state, &user, &note_id, &username).await
}

/// Legacy alias: `/notes/:id` → owner's note using the legacy `/notes/:id`
/// canonical ID shape (preserves URLs of already-published notes).
pub async fn note_handler(
    State(state): State<AppState>,
    Path(note_uuid): Path<String>,
) -> AppResult<Response> {
    let username = state.owner_key.username.clone();
    let user = state.find_user(&username).await?;
    let note_id = format!("{}/notes/{note_uuid}", state.config.base_url());
    serve_note(&state, &user, &note_id, &username).await
}

async fn serve_note(
    state: &AppState,
    user: &LocalUser,
    note_id: &str,
    username: &str,
) -> AppResult<Response> {
    let row = sqlx::query_as::<_, (Option<String>, String, String, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT title, url, content, registered_at, updated_at \
         FROM posts WHERE ap_note_id = $1 AND user_id = $2 AND active = TRUE",
    )
    .bind(note_id)
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?;

    let (title, url, content_md, registered_at, updated_at) = row.ok_or(AppError::NotFound)?;

    let actor_url = state.config.user_actor_url(username);
    let followers_url = state.config.user_followers_url(username);
    let published_str = registered_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let updated_str = updated_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    use crate::ap::publish::render_note_html;
    let content_html = render_note_html(title.as_deref(), &url, &content_md);

    let mut note = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id":           note_id,
        "type":         "Note",
        "attributedTo": actor_url,
        "content":      content_html,
        "url":          url,
        "published":    published_str,
        "to":  [PUBLIC_URI],
        "cc":  [followers_url],
        "source": {
            "content":   content_md,
            "mediaType": "text/markdown",
        },
    });
    if updated_at > registered_at + Duration::seconds(5) {
        note["updated"] = serde_json::Value::String(updated_str);
    }

    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/activity+json; charset=utf-8",
        )],
        Json(note),
    )
        .into_response())
}

/// Returns true if the Accept header indicates an AP/JSON-LD client.
fn wants_ap(accept: &str) -> bool {
    accept.contains("application/activity+json")
        || accept.contains("application/ld+json")
        || accept.contains("application/json")
}
