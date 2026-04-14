use crate::{
    ap::fetch::build_http_client,
    config::Config,
    error::{AppError, AppResult},
};
use anyhow::{Context, Result};
use deadpool_redis::{Config as RedisConfig, Pool as RedisPool, Runtime};
use reqwest::Client;
use rsa::{
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    RsaPrivateKey, RsaPublicKey,
};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// A local user's public-facing profile: everything needed to serve an actor
/// document without touching the private key.
#[derive(Clone, Debug)]
pub struct LocalUser {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub summary: String,
    pub public_key_pem: String,
}

/// An RSA keypair belonging to a single local user (used to sign that user's
/// outbound ActivityPub requests).
#[derive(Clone)]
pub struct UserKey {
    pub user_id: Uuid,
    pub username: String,
    pub private_key: Arc<RsaPrivateKey>,
    #[allow(dead_code)]
    pub public_key: Arc<RsaPublicKey>,
    pub public_key_pem: String,
}

/// Shared application state, cheaply cloneable via Arc internals.
///
/// In the single-actor OSS deployment, `owner_user_id` and `owner_key` point
/// at the local user derived from `[instance] username` in the config. The
/// SaaS layer ignores these and resolves the active user per-request.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub redis: RedisPool,
    /// The user corresponding to `config.instance.username` in OSS mode.
    /// Used as the default owner for any legacy/single-actor route.
    pub owner_user_id: Uuid,
    pub owner_key: Arc<UserKey>,
    /// Shared HTTP client for outbound AP fetches (signed GETs/POSTs).
    pub http: Client,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self> {
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.database.max_connections)
            .connect(&config.database.url)
            .await
            .context("connecting to PostgreSQL")?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&db)
            .await
            .context("running database migrations")?;

        let redis_cfg = RedisConfig::from_url(&config.redis.url);
        let redis = redis_cfg
            .create_pool(Some(Runtime::Tokio1))
            .context("creating Redis pool")?;

        let owner_key = load_or_bootstrap_owner(&db, &config.instance.username).await?;
        let owner_user_id = owner_key.user_id;
        let http = build_http_client().context("building HTTP client")?;

        Ok(Self {
            config: Arc::new(config),
            db,
            redis,
            owner_user_id,
            owner_key: Arc::new(owner_key),
            http,
        })
    }
}

/// Ensure a user named `username` exists with a keypair, and return it.
///
/// Precedence:
///   1. If a user with this username already exists, use it (load its key).
///   2. Else if a `_bootstrap` user exists (legacy singleton migration),
///      rename it to `username`.
///   3. Else create a fresh user with a freshly generated RSA keypair.
async fn load_or_bootstrap_owner(db: &PgPool, username: &str) -> Result<UserKey> {
    // 1. existing user with this username
    if let Some(key) = load_user_key_by_username(db, username).await? {
        tracing::info!("loaded existing keypair for @{username}");
        return Ok(key);
    }

    // 2. legacy singleton migration: rename _bootstrap → config username
    let bootstrap_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE username = '_bootstrap'")
            .fetch_optional(db)
            .await
            .context("checking for _bootstrap user")?;
    if let Some(id) = bootstrap_id {
        sqlx::query(
            "UPDATE users SET username = $1, display_name = $2, updated_at = NOW() WHERE id = $3",
        )
        .bind(username)
        .bind(username)
        .bind(id)
        .execute(db)
        .await
        .context("renaming _bootstrap user")?;

        tracing::info!("migrated legacy singleton → user @{username}");
        return load_user_key_by_username(db, username)
            .await?
            .context("post-migration key lookup failed");
    }

    // 3. fresh install: create a new user with a freshly generated keypair
    tracing::info!("creating new local user @{username} with fresh 2048-bit RSA keypair…");
    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048).context("generating RSA key")?;

    let priv_pem_z = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .context("encoding private key PEM")?;
    let private_pem: String = priv_pem_z.as_str().to_string();

    let public_pem: String = private_key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .context("encoding public key PEM")?;

    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, display_name) VALUES ($1, $1) RETURNING id",
    )
    .bind(username)
    .fetch_one(db)
    .await
    .context("creating user row")?;

    sqlx::query(
        "INSERT INTO user_keys (user_id, private_key_pem, public_key_pem) VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(&private_pem)
    .bind(&public_pem)
    .execute(db)
    .await
    .context("storing user keypair")?;

    let public_key = Arc::new(
        RsaPublicKey::from_public_key_pem(&public_pem).context("re-parsing public key")?,
    );

    Ok(UserKey {
        user_id,
        username: username.to_string(),
        private_key: Arc::new(private_key),
        public_key,
        public_key_pem: public_pem,
    })
}

async fn load_user_key_by_username(db: &PgPool, username: &str) -> Result<Option<UserKey>> {
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT u.id, k.private_key_pem, k.public_key_pem \
         FROM users u JOIN user_keys k ON k.user_id = u.id \
         WHERE u.username = $1",
    )
    .bind(username)
    .fetch_optional(db)
    .await
    .context("loading user keypair")?;

    let Some((user_id, private_pem, public_pem)) = row else {
        return Ok(None);
    };

    let private_key = Arc::new(
        RsaPrivateKey::from_pkcs8_pem(&private_pem).context("parsing private key PEM")?,
    );
    let public_key = Arc::new(
        RsaPublicKey::from_public_key_pem(&public_pem).context("parsing public key PEM")?,
    );

    Ok(Some(UserKey {
        user_id,
        username: username.to_string(),
        private_key,
        public_key,
        public_key_pem: public_pem,
    }))
}

// ── Per-request user resolution (Phase 2+) ───────────────────────────────────

impl AppState {
    /// Look up a local user by their username. Returns `AppError::NotFound`
    /// if no user with that name exists. Case-insensitive because
    /// `users.username` is `CITEXT`.
    pub async fn find_user(&self, username: &str) -> AppResult<LocalUser> {
        let row = sqlx::query_as::<_, (Uuid, String, String, String, String)>(
            "SELECT u.id, u.username::text, u.display_name, u.summary, k.public_key_pem \
             FROM users u JOIN user_keys k ON k.user_id = u.id \
             WHERE u.username = $1",
        )
        .bind(username)
        .fetch_optional(&self.db)
        .await
        .map_err(AppError::from)?;

        let (id, username, display_name, summary, public_key_pem) =
            row.ok_or(AppError::NotFound)?;
        Ok(LocalUser {
            id,
            username,
            display_name,
            summary,
            public_key_pem,
        })
    }

    /// Load the full keypair for a user, given their id. This is only needed
    /// for outbound operations (publish/send_accept) that sign on the user's
    /// behalf. Reading private-key material from the DB on every hot-path
    /// request is intentional for now; a later phase can add a Redis-backed
    /// key cache keyed by user_id.
    pub async fn load_user_key(&self, user_id: Uuid) -> AppResult<UserKey> {
        let row = sqlx::query_as::<_, (String, String, String)>(
            "SELECT u.username::text, k.private_key_pem, k.public_key_pem \
             FROM users u JOIN user_keys k ON k.user_id = u.id \
             WHERE u.id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(AppError::from)?;

        let (username, private_pem, public_pem) = row.ok_or(AppError::NotFound)?;

        let private_key = Arc::new(
            RsaPrivateKey::from_pkcs8_pem(&private_pem)
                .map_err(|e| AppError::Crypto(format!("parsing private key: {e}")))?,
        );
        let public_key = Arc::new(
            RsaPublicKey::from_public_key_pem(&public_pem)
                .map_err(|e| AppError::Crypto(format!("parsing public key: {e}")))?,
        );

        Ok(UserKey {
            user_id,
            username,
            private_key,
            public_key,
            public_key_pem: public_pem,
        })
    }
}
