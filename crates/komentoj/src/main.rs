use komentoj_core::{build_router, AppState, Config};
use std::net::SocketAddr;
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
    let config = Config::load(&config_path)?;

    tracing::info!(
        "starting komentoj for @{}@{}",
        config.instance.username,
        config.instance.domain
    );

    let host = config.server.host.clone();
    let port = config.server.port;

    let state = AppState::new(config).await?;
    let app = build_router(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
