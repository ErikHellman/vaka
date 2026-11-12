//! Authentication and authorization.
//!
//! Two kinds of principals:
//! - Users (web UI), authenticated by a session cookie, with an instance-wide role.
//! - API tokens (REST API and MCP), `Authorization: Bearer vt_...`, scoped to one
//!   project or all projects, and to `read` or `write`.

use crate::db::RowExt;
use crate::state::AppState;
use crate::util::{new_id, now_ms, random_token, sha256_hex};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;

pub const SESSION_COOKIE: &str = "vaka_session";
const SESSION_TTL_MS: i64 = 30 * 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer = 0,
    Member = 1,
    Admin = 2,
    Owner = 3,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "member" => Some(Role::Member),
            "admin" => Some(Role::Admin),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Principal {
    User { id: String, email: String, name: String, role: Role },
    Token { id: String, name: String, project_id: Option<String>, write: bool },
}

impl Principal {
    pub fn label(&self) -> String {
        match self {
            Principal::User { email, .. } => email.clone(),
            Principal::Token { name, .. } => format!("token:{name}"),
        }
    }

    pub fn can_read(&self, project_id: &str) -> bool {
        match self {
            Principal::User { .. } => true,
            Principal::Token { project_id: p, .. } => p.as_deref().map(|p| p == project_id).unwrap_or(true),
        }
    }

    /// Triage-level writes: issue status, comments, artifacts, alert rules.
    pub fn can_write(&self, project_id: &str) -> bool {
        match self {
            Principal::User { role, .. } => *role >= Role::Member,
            Principal::Token { write, .. } => *write && self.can_read(project_id),
        }
    }

    /// Instance administration: users, tokens, project settings.
    pub fn is_admin(&self) -> bool {
        matches!(self, Principal::User { role, .. } if *role >= Role::Admin)
    }

    /// Creating projects is allowed for admins and all-project write tokens (agent setup).
    pub fn can_create_project(&self) -> bool {
        match self {
            Principal::User { role, .. } => *role >= Role::Admin,
            Principal::Token { project_id, write, .. } => *write && project_id.is_none(),
        }
    }

    pub fn project_scope(&self) -> Option<&str> {
        match self {
            Principal::Token { project_id, .. } => project_id.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn not_found(what: &str) -> Self {
        ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
    pub fn forbidden() -> Self {
        ApiError(StatusCode::FORBIDDEN, "this token or user is not allowed to do that".into())
    }
    pub fn bad(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("internal error: {e:#}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("internal error: {e}"))
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::from(anyhow::Error::from(e))
    }
}

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?
        .to_string())
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub fn cookie_value(parts: &Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub async fn principal_from_token(state: &AppState, token: &str) -> anyhow::Result<Option<Principal>> {
    let now = now_ms();
    let row = sqlx::query("SELECT id, name, project_id, scope, expires_at FROM api_tokens WHERE token_hash = $1 AND revoked_at IS NULL")
        .bind(sha256_hex(token))
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else { return Ok(None) };
    if row.oi("expires_at").map(|e| e < now).unwrap_or(false) {
        return Ok(None);
    }
    sqlx::query("UPDATE api_tokens SET last_used_at = $1 WHERE id = $2")
        .bind(now)
        .bind(row.s("id"))
        .execute(&state.db)
        .await
        .ok();
    Ok(Some(Principal::Token {
        id: row.s("id"),
        name: row.s("name"),
        project_id: row.os("project_id").filter(|p| !p.is_empty()),
        write: row.s("scope") == "write",
    }))
}

pub async fn principal_from_session(state: &AppState, token: &str) -> anyhow::Result<Option<Principal>> {
    let row = sqlx::query(
        "SELECT u.id, u.email, u.name, u.role FROM web_sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.expires_at > $2 AND u.disabled = 0",
    )
    .bind(sha256_hex(token))
    .bind(now_ms())
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| Principal::User {
        id: r.s("id"),
        email: r.s("email"),
        name: r.s("name"),
        role: Role::parse(&r.s("role")).unwrap_or(Role::Viewer),
    }))
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let bearer = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|s| s.trim().to_string());
        if let Some(token) = bearer {
            return principal_from_token(state, &token)
                .await?
                .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "invalid, expired or revoked API token".into()));
        }
        if let Some(session) = cookie_value(parts, SESSION_COOKIE) {
            if let Some(p) = principal_from_session(state, &session).await? {
                return Ok(p);
            }
        }
        Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "missing credentials: send `Authorization: Bearer <api token>` (create one in the admin UI or with `vaka token create`)".into(),
        ))
    }
}

pub async fn create_session(state: &AppState, user_id: &str) -> anyhow::Result<String> {
    let token = random_token(32);
    let now = now_ms();
    sqlx::query("INSERT INTO web_sessions (token_hash, user_id, created_at, expires_at) VALUES ($1, $2, $3, $4)")
        .bind(sha256_hex(&token))
        .bind(user_id)
        .bind(now)
        .bind(now + SESSION_TTL_MS)
        .execute(&state.db)
        .await?;
    Ok(token)
}

pub fn session_cookie(state: &AppState, token: &str, max_age_s: i64) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_s}{}",
        if state.config.secure_cookies { "; Secure" } else { "" }
    )
}

pub async fn create_user(state: &AppState, email: &str, name: &str, password: &str, role: Role) -> anyhow::Result<String> {
    let id = new_id();
    sqlx::query("INSERT INTO users (id, email, name, password_hash, role, created_at, disabled) VALUES ($1, $2, $3, $4, $5, $6, 0)")
        .bind(&id)
        .bind(email.trim().to_lowercase())
        .bind(name)
        .bind(hash_password(password)?)
        .bind(role.as_str())
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    Ok(id)
}

/// Create the initial owner from VAKA_ADMIN_EMAIL / VAKA_ADMIN_PASSWORD when no users exist.
/// With email configured, VAKA_ADMIN_EMAIL alone is enough: the owner logs in with an email code.
pub async fn bootstrap_admin(state: &AppState) -> anyhow::Result<()> {
    if !state.config.password_login && !state.config.email_login() {
        tracing::warn!("VAKA_PASSWORD_LOGIN=false is ignored because email is not configured (set VAKA_RESEND_API_KEY)");
    }
    let count = sqlx::query("SELECT COUNT(*) AS n FROM users").fetch_one(&state.db).await?.i("n");
    if count > 0 {
        return Ok(());
    }
    match (&state.config.admin_email, &state.config.admin_password) {
        (Some(email), Some(pw)) => {
            if pw.len() < 8 {
                anyhow::bail!("VAKA_ADMIN_PASSWORD must be at least 8 characters");
            }
            create_user(state, email, "Admin", pw, Role::Owner).await?;
            tracing::info!(%email, "created initial owner account");
        }
        (Some(email), None) if state.config.email_login() => {
            // Random password nobody knows; the owner signs in with an email code.
            create_user(state, email, "Admin", &random_token(32), Role::Owner).await?;
            tracing::info!(%email, "created initial owner account (email code login)");
        }
        _ => tracing::warn!(
            "no users exist: set VAKA_ADMIN_EMAIL and VAKA_ADMIN_PASSWORD (or VAKA_ADMIN_EMAIL plus VAKA_RESEND_API_KEY) to create the first owner"
        ),
    }
    Ok(())
}

pub struct NewToken {
    pub id: String,
    pub token: String,
}

pub async fn create_api_token(
    state: &AppState,
    name: &str,
    project_id: Option<&str>,
    write: bool,
    expires_at: Option<i64>,
    created_by: &str,
) -> anyhow::Result<NewToken> {
    let id = new_id();
    let token = format!("vt_{}", random_token(24));
    sqlx::query(
        "INSERT INTO api_tokens (id, name, token_hash, token_prefix, project_id, scope, created_by, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&id)
    .bind(name)
    .bind(sha256_hex(&token))
    .bind(&token[..10])
    .bind(project_id)
    .bind(if write { "write" } else { "read" })
    .bind(created_by)
    .bind(now_ms())
    .bind(expires_at)
    .execute(&state.db)
    .await?;
    Ok(NewToken { id, token })
}

pub async fn audit(state: &AppState, actor: &str, action: &str, target: &str, detail: &str) {
    let _ = sqlx::query("INSERT INTO audit_log (id, actor, action, target, detail, created_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(new_id())
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(detail)
        .bind(now_ms())
        .execute(&state.db)
        .await;
}
