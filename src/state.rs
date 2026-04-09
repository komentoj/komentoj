use crate::{ap::fetch::build_http_client, config::Config};
use anyhow::{Context, Result};
use deadpool_redis::{Config as RedisConfig, Pool as RedisPool, Runtime};
use reqwest::Client;
use rsa::{
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    RsaPrivateKey, RsaPublicKey,
};
use sqlx::PgPool;
use std::sync::Arc;

/// The RSA keypair for this instance (used to sign outbound requests)
#[derive(Clone)]
pub struct InstanceKey {
    pub private_key: Arc<RsaPrivateKey>,
    #[allow(dead_code)]
    pub public_key: Arc<RsaPublicKey>,
    pub public_key_pem: String,
}

/// Shared application state, cheaply cloneable via Arc internals
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub redis: RedisPool,
    pub key: Arc<InstanceKey>,
    /// Shared HTTP client for outbound AP fetches (signed GETs/POSTs)
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

        let key = load_or_generate_key(&db).await?;
        let http = build_http_client().context("building HTTP client")?;

        Ok(Self {
            config: Arc::new(config),
            db,
            redis,
            key: Arc::new(key),
            http,
        })
    }
}

/// Load the instance keypair from the database, generating a new one if absent.
async fn load_or_generate_key(db: &PgPool) -> Result<InstanceKey> {
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT private_key_pem, public_key_pem FROM instance_keys WHERE id = 1",
    )
    .fetch_optional(db)
    .await
    .context("fetching instance key")?;

    let (private_pem, public_pem): (String, String) = if let Some(row) = row {
        tracing::info!("loaded existing instance keypair from database");
        row
    } else {
        tracing::info!("generating new 2048-bit RSA keypair…");
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).context("generating RSA key")?;

        // Zeroizing<String> → deref to String
        let priv_pem_z = private_key
            .to_pkcs8_pem(LineEnding::LF)
            .context("encoding private key PEM")?;
        let private_pem: String = priv_pem_z.as_str().to_string();

        let public_pem: String = private_key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .context("encoding public key PEM")?;

        sqlx::query(
            "INSERT INTO instance_keys (id, private_key_pem, public_key_pem) VALUES (1, $1, $2)",
        )
        .bind(&private_pem)
        .bind(&public_pem)
        .execute(db)
        .await
        .context("storing instance key")?;

        tracing::info!("new keypair stored in database");
        (private_pem, public_pem)
    };

    let private_key =
        RsaPrivateKey::from_pkcs8_pem(&private_pem).context("parsing private key PEM")?;
    let public_key =
        RsaPublicKey::from_public_key_pem(&public_pem).context("parsing public key PEM")?;

    Ok(InstanceKey {
        private_key: Arc::new(private_key),
        public_key: Arc::new(public_key),
        public_key_pem: public_pem,
    })
}
