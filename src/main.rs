mod ap;
mod api;
mod config;
mod error;
mod state;

#[cfg(test)]
mod test_helpers;

use crate::{
    ap::{
        actor::{
            actor_handler, followers_handler, following_handler, note_handler, outbox_handler,
            webfinger_handler,
        },
        inbox::inbox_handler,
    },
    api::{comments::get_comments, posts::sync_posts},
    state::AppState,
};
use axum::{
    http::{HeaderValue, Method},
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use tower_http::{
    cors::{AllowHeaders, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "komentoj=info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Config
    let config_path = std::env::var("KOMENTOJ_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let config = config::Config::load(&config_path)?;

    tracing::info!(
        "starting komentoj for @{}@{}",
        config.instance.username,
        config.instance.domain
    );

    let allowed_origins: Vec<HeaderValue> = config
        .cors
        .allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    let state = AppState::new(config).await?;

    // CORS for the public /api/* routes (blog frontends need this)
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods([Method::GET, Method::OPTIONS])
        .allow_headers(AllowHeaders::any());

    let app = Router::new()
        // ActivityPub / Fediverse discovery
        .route("/.well-known/webfinger", get(webfinger_handler))
        .route("/actor", get(actor_handler))
        .route("/inbox", post(inbox_handler))
        .route("/outbox", get(outbox_handler))
        .route("/followers", get(followers_handler))
        .route("/following", get(following_handler))
        // Individual Note documents (fetched by remote AP servers to verify replies)
        .route("/notes/{id}", get(note_handler))
        // Public REST API for the blog frontend
        .route("/api/v1/comments", get(get_comments))
        // Admin API (requires Bearer token)
        .route("/api/v1/posts/sync", post(sync_posts))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr: SocketAddr =
        format!("{}:{}", state.config.server.host, state.config.server.port).parse()?;

    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
