//! Admin API — user management.
//!
//! POST /api/v1/admin/users   — create a new local user (generates keypair,
//!                              returns api_token for subsequent per-user calls)
//! GET  /api/v1/admin/users   — list registered users
//! DELETE /api/v1/admin/users/:username — remove a user (cascades posts/followers)
//!
//! All endpoints require the global admin bearer token.
//!
//! The SaaS layer should mount its own auth layer (Supabase JWT) in front of
//! these or, more typically, bypass them and drive provisioning from its own
//! webhook handlers; the routes here exist for self-hosted OSS operators.

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use rsa::{
    pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding},
    RsaPrivateKey,
};
// Keep spawn_blocking contained within create_user
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

fn require_admin(state: &AppState, bearer: &Bearer) -> AppResult<()> {
    if bool::from(
        bearer
            .token()
            .as_bytes()
            .ct_eq(state.config.admin.token.as_bytes()),
    ) {
        Ok(())
    } else {
        Err(AppError::Unauthorized("invalid admin token".into()))
    }
}

// ── Create user ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub summary: String,
    /// Initial subscription tier. Defaults to "self_host" for OSS installs.
    #[serde(default)]
    pub plan_tier: Option<String>,
}

#[derive(Serialize)]
pub struct CreateUserResponse {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub summary: String,
    pub plan_tier: String,
    /// Fresh per-user bearer token. Keep it safe — it grants write access to
    /// this user's posts/sync. Regenerate by deleting + recreating the user.
    pub api_token: String,
}

pub async fn create_user(
    State(state): State<AppState>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    Json(req): Json<CreateUserRequest>,
) -> AppResult<Json<CreateUserResponse>> {
    require_admin(&state, &bearer)?;

    let username = req.username.trim().to_string();
    if username.is_empty() {
        return Err(AppError::BadRequest("username must not be empty".into()));
    }
    // Basic sanity check: no slashes, no @, no whitespace. Lowercase letters,
    // digits, hyphen, underscore only. Matches Mastodon-ish conventions.
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::BadRequest(
            "username may contain only [A-Za-z0-9_-]".into(),
        ));
    }

    let display_name = if req.display_name.is_empty() {
        username.clone()
    } else {
        req.display_name
    };
    let plan_tier = req.plan_tier.unwrap_or_else(|| "self_host".into());

    // Generate RSA-2048 keypair on a blocking thread so we don't stall the
    // async runtime (~hundreds of ms of CPU) and so the `!Send` thread-local
    // RNG never crosses an await point.
    let (private_pem, public_pem) = tokio::task::spawn_blocking(|| {
        let mut rng = rand::thread_rng();
        let pk = RsaPrivateKey::new(&mut rng, 2048)
            .map_err(|e| AppError::Crypto(format!("RSA keygen: {e}")))?;
        let priv_pem = pk
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|e| AppError::Crypto(format!("encode private: {e}")))?
            .as_str()
            .to_string();
        let pub_pem = pk
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| AppError::Crypto(format!("encode public: {e}")))?;
        Ok::<(String, String), AppError>((priv_pem, pub_pem))
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("RSA keygen join error: {e}")))??;

    // Generate api_token (32 bytes → 64 hex chars).
    let api_token = generate_token_hex(32);

    // Insert user + key atomically
    let mut tx = state.db.begin().await?;

    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, display_name, summary, api_token, plan_tier) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&username)
    .bind(&display_name)
    .bind(&req.summary)
    .bind(&api_token)
    .bind(&plan_tier)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(dbe) if dbe.is_unique_violation() => {
            AppError::BadRequest(format!("username '{username}' already exists"))
        }
        other => AppError::from(other),
    })?;

    sqlx::query(
        "INSERT INTO user_keys (user_id, private_key_pem, public_key_pem) VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(&private_pem)
    .bind(&public_pem)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(CreateUserResponse {
        id: user_id,
        username,
        display_name,
        summary: req.summary,
        plan_tier,
        api_token,
    }))
}

// ── List users ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub plan_tier: String,
}

pub async fn list_users(
    State(state): State<AppState>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
) -> AppResult<Json<Vec<UserSummary>>> {
    require_admin(&state, &bearer)?;

    let rows = sqlx::query_as::<_, (Uuid, String, String, String)>(
        "SELECT id, username::text, display_name, plan_tier FROM users \
         WHERE username <> '_bootstrap' ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await?;

    let out = rows
        .into_iter()
        .map(|(id, username, display_name, plan_tier)| UserSummary {
            id,
            username,
            display_name,
            plan_tier,
        })
        .collect();

    Ok(Json(out))
}

// ── Delete user ──────────────────────────────────────────────────────────────

pub async fn delete_user(
    State(state): State<AppState>,
    Path(username): Path<String>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
) -> AppResult<Json<serde_json::Value>> {
    require_admin(&state, &bearer)?;

    // Guard: deleting the owner would leave the legacy routes pointing at
    // nothing. Require caller to change config.instance.username first.
    if username == state.owner_key.username {
        return Err(AppError::BadRequest(
            "cannot delete the configured owner user".into(),
        ));
    }

    let rows = sqlx::query("DELETE FROM users WHERE username = $1")
        .bind(&username)
        .execute(&state.db)
        .await?
        .rows_affected();

    if rows == 0 {
        return Err(AppError::NotFound);
    }

    Ok(Json(serde_json::json!({ "deleted": username })))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn generate_token_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex_encode(&buf)
}

fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &byte in b {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}
