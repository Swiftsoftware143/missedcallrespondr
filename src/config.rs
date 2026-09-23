use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub jwt_secret: String,
    pub server_port: u16,
    pub server_host: String,
    pub internal_sync_key: String,
    pub funnelswift_url: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            // Placeholder only: main() awaits PgPool::connect() before binding, so a missing
            // DATABASE_URL fails loudly at startup (see the connection error), never silently.
            database_url: std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgres://swift:swift@localhost:5432/missedcallrespondr".into()
            }),
            // Secret: no fallback. A literal here would be a signing key published in this
            // public repo, so a missing JWT_SECRET must stop the process instead.
            jwt_secret: required_secret("JWT_SECRET"),
            // Bind address comes from the deploy env, same keys as the rest of the fleet
            // (ADASwift/IncentiveSwift read HOST/PORT; /etc/swift/env/missedcall.env supplies
            // HOST=127.0.0.1). The old SERVER_HOST/SERVER_PORT names nothing ever set, so the
            // service ignored the operator and bound 0.0.0.0:8088 on every interface.
            server_port: std::env::var("PORT")
                .unwrap_or_else(|_| "8088".into())
                .parse()
                .unwrap_or(8088),
            server_host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            // Secret: no fallback and no empty value. The internal endpoints compare this
            // against the X-Internal-Key header, so an empty key would authorise a request
            // that simply omits the header.
            internal_sync_key: required_secret("INTERNAL_SYNC_KEY"),
            funnelswift_url: std::env::var("FUNNELSWIFT_URL")
                .unwrap_or_else(|_| "http://localhost:8080".into()),
        }
    }
}

/// Read a secret-shaped env var: it must be present and non-empty, otherwise the process
/// must not start. A silent fallback here would either ship a signing key that is published
/// in this repo, or let an internal-key gate compare "" against a request that omits the
/// header. Same shape as WorkflowSwift/src/config.rs.
fn required_secret(key: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) => panic!("{key} environment variable must not be empty"),
        Err(_) => panic!("{key} environment variable is required"),
    }
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct Account {
    pub id: uuid::Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct TeamMember {
    pub id: uuid::Uuid,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub tenant_id: uuid::Uuid,
    pub role: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: uuid::Uuid,
    pub email: String,
    pub aid: uuid::Uuid,
    pub role: String,
    pub exp: usize,
    pub iat: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
    pub account_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub team_member: TeamMemberResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TeamMemberResponse {
    pub id: uuid::Uuid,
    pub email: String,
    pub name: String,
    #[serde(rename = "account_id")]
    pub tenant_id: uuid::Uuid,
    pub role: String,
}

impl From<TeamMember> for TeamMemberResponse {
    fn from(u: TeamMember) -> Self {
        Self {
            id: u.id,
            email: u.email,
            name: u.name,
            tenant_id: u.tenant_id,
            role: u.role,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}
