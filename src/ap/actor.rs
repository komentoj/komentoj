//! Actor and WebFinger endpoints.
//!
//! GET /.well-known/webfinger?resource=acct:comments@domain
//! GET /actor

use crate::{
    ap::types::{
        actor_context, ActorDocument, ActorEndpointsOut, PublicKeyObject, WebFingerLink,
        WebFingerResponse, PUBLIC_URI,
    },
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
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

    // Accept both "acct:username@domain" and the bare actor URL
    let expected_acct = state.config.acct();
    let expected_actor = state.config.actor_url();

    if resource != expected_acct && resource != expected_actor {
        return Err(AppError::NotFound);
    }

    let jrd = WebFingerResponse {
        subject: expected_acct,
        aliases: vec![expected_actor.clone()],
        links: vec![
            WebFingerLink {
                rel: "http://webfinger.net/rel/profile-page".into(),
                link_type: Some("text/html".into()),
                href: Some(expected_actor.clone()),
            },
            WebFingerLink {
                rel: "self".into(),
                link_type: Some("application/activity+json".into()),
                href: Some(expected_actor),
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

// ── Actor ─────────────────────────────────────────────────────────────────────

pub async fn actor_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> AppResult<Response> {
    // Content-negotiate: serve HTML for browsers, JSON-LD for AP clients
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/html");

    if !wants_ap(accept) {
        // Redirect browsers to the blog or a simple info page
        return Ok((
            StatusCode::SEE_OTHER,
            [(
                header::LOCATION,
                format!("https://{}", state.config.instance.domain),
            )],
        )
            .into_response());
    }

    let domain = &state.config.instance.domain;
    let actor_url = state.config.actor_url();
    let key_id = state.config.key_id();

    let doc = ActorDocument {
        context: actor_context(),
        id: actor_url.clone(),
        actor_type: "Service",  // bot/service account
        preferred_username: state.config.instance.username.clone(),
        name: state.config.instance.display_name.clone(),
        summary: state.config.instance.summary.clone(),
        url: actor_url.clone(),
        inbox: state.config.inbox_url(),
        outbox: format!("https://{domain}/outbox"),
        followers: format!("https://{domain}/followers"),
        following: format!("https://{domain}/following"),
        endpoints: ActorEndpointsOut {
            shared_inbox: state.config.inbox_url(),
        },
        public_key: PublicKeyObject {
            id: key_id,
            owner: Some(actor_url),
            public_key_pem: state.key.public_key_pem.clone(),
        },
        manually_approves_followers: false,
        discoverable: true,
        published: "2024-01-01T00:00:00Z".into(), // stable value; doesn't need to be exact
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

/// Stub endpoints required by AP spec (empty ordered collections)
pub async fn outbox_handler(State(state): State<AppState>) -> impl IntoResponse {
    let domain = &state.config.instance.domain;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("https://{domain}/outbox"),
            "type": "OrderedCollection",
            "totalItems": 0,
            "first": format!("https://{domain}/outbox?page=1"),
        })),
    )
}

pub async fn followers_handler(State(state): State<AppState>) -> impl IntoResponse {
    let domain = &state.config.instance.domain;
    let count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM followers WHERE accepted = TRUE",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("https://{domain}/followers"),
            "type": "OrderedCollection",
            "totalItems": count,
        })),
    )
}

pub async fn following_handler(State(state): State<AppState>) -> impl IntoResponse {
    let domain = &state.config.instance.domain;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/activity+json")],
        Json(serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("https://{domain}/following"),
            "type": "OrderedCollection",
            "totalItems": 0,
        })),
    )
}

// ── Helpers ───────────────────────────────────────────────────────────────────

// ── Note fetch endpoint ───────────────────────────────────────────────────────

/// GET /notes/:note_uuid
///
/// Remote AP servers fetch this URL to verify the Note exists when processing
/// a reply. We reconstruct the Note from the posts table on demand.
pub async fn note_handler(
    State(state): State<AppState>,
    Path(note_uuid): Path<String>,
) -> AppResult<Response> {
    let note_id = format!(
        "https://{}/notes/{note_uuid}",
        state.config.instance.domain
    );

    let row = sqlx::query_as::<_, (Option<String>, String, String, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT title, url, content, registered_at, updated_at \
         FROM posts WHERE ap_note_id = $1 AND active = TRUE",
    )
    .bind(&note_id)
    .fetch_optional(&state.db)
    .await?;

    let (title, url, content_md, registered_at, updated_at) = row.ok_or(AppError::NotFound)?;

    let domain = &state.config.instance.domain;
    let published_str = registered_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let updated_str   = updated_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    use crate::ap::publish::render_note_html;
    let content_html = render_note_html(title.as_deref(), &url, &content_md);

    let mut note = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id":           note_id,
        "type":         "Note",
        "attributedTo": state.config.actor_url(),
        "content":      content_html,
        "url":          url,
        "published":    published_str,
        "to":  [PUBLIC_URI],
        "cc":  [format!("https://{domain}/followers")],
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
        [(header::CONTENT_TYPE, "application/activity+json; charset=utf-8")],
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
