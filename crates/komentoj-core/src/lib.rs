//! Core library for komentoj — an ActivityPub comment server.
//!
//! Exposes configuration loading, shared application state, the router builder,
//! and all AP/API handlers. A thin binary wrapper (the `komentoj` crate) is
//! responsible only for initializing tracing and starting the HTTP server.

pub mod ap;
pub mod api;
pub mod config;
pub mod error;
pub mod state;

#[cfg(test)]
pub(crate) mod test_helpers;

pub use config::Config;
pub use state::AppState;

use crate::{
    ap::{
        actor::{
            actor_handler, followers_handler, following_handler, note_handler, outbox_handler,
            user_actor_handler, user_followers_handler, user_following_handler, user_note_handler,
            user_outbox_handler, webfinger_handler,
        },
        inbox::{inbox_handler, user_inbox_handler},
    },
    api::{comments::get_comments, posts::sync_posts},
};
use axum::{
    http::{HeaderValue, Method},
    routing::{get, post},
    Router,
};
use tower_http::{
    cors::{AllowHeaders, CorsLayer},
    trace::TraceLayer,
};

/// Build the application router with all ActivityPub and public API routes
/// wired to the provided state.
///
/// This is the single entry point a binary or SaaS wrapper should use to
/// obtain a fully-configured `axum::Router`. Callers are free to layer
/// additional middleware (auth, quota, tenancy) on top of the returned router.
pub fn build_router(state: AppState) -> Router {
    let allowed_origins: Vec<HeaderValue> = state
        .config
        .cors
        .allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    // CORS for the public /api/* routes (blog frontends need this)
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods([Method::GET, Method::OPTIONS])
        .allow_headers(AllowHeaders::any());

    Router::new()
        // ── Per-user ActivityPub routes (canonical) ──────────────────────────
        .route("/users/{username}", get(user_actor_handler))
        .route("/users/{username}/inbox", post(user_inbox_handler))
        .route("/users/{username}/outbox", get(user_outbox_handler))
        .route("/users/{username}/followers", get(user_followers_handler))
        .route("/users/{username}/following", get(user_following_handler))
        .route(
            "/users/{username}/notes/{note_uuid}",
            get(user_note_handler),
        )
        // ── Legacy single-actor aliases (resolve to config.instance.username) ─
        .route("/.well-known/webfinger", get(webfinger_handler))
        .route("/actor", get(actor_handler))
        .route("/inbox", post(inbox_handler))
        .route("/outbox", get(outbox_handler))
        .route("/followers", get(followers_handler))
        .route("/following", get(following_handler))
        .route("/notes/{id}", get(note_handler))
        // ── Public REST API for the blog frontend ────────────────────────────
        .route("/api/v1/comments", get(get_comments))
        // ── Admin API (requires Bearer token) ────────────────────────────────
        .route("/api/v1/posts/sync", post(sync_posts))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
