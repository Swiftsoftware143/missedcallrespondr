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
    /// PayPal's public webhook identifier (PayPal dashboard → app → Webhooks) used by the
    /// `POST /api/v1/webhooks/paypal` receiver to verify `paypal-transmission-sig` against
    /// PayPal's `verify-webhook-signature` API.
    ///
    /// NOT a secret and NOT the shared internal sync key: it names WHICH webhook configuration
    /// PayPal must verify against, so it must be independent of INTERNAL_SYNC_KEY and rotating
    /// that credential cannot invalidate webhook verification (the shape ADASwift
    /// t_2be56050/t_9a1da415 shipped).
    ///
    /// Optional on purpose: unset is not an outage, it is an unconfigured receiver — the webhook
    /// then answers `503 paypal_not_configured` and processes nothing (kanban t_5cf44e1b).
    /// Resolution order is this value, then the active `paypal` provider row's `webhook_secret`
    /// (the field the admin console's Payment providers panel writes), so PayPal can be enabled
    /// from the console without a redeploy.
    pub paypal_webhook_id: String,
    /// This deployment's Telnyx ACCOUNT Ed25519 public key (base64, 32 bytes), from Telnyx Mission
    /// Control. The credential `POST /api/v1/telnyx/webhook` verifies every delivery against
    /// (kanban t_0e4ae7b7).
    ///
    /// There is NO universal Telnyx public key — it is per account — so the receiver cannot carry
    /// one and must be configured. Optional on purpose: unset is not an outage, it is an
    /// UNCONFIGURED receiver, and the receiver answers `503 telnyx_verification_not_configured`
    /// and applies NOTHING (fail closed). A delivery that cannot be verified is never acted on.
    /// `TELNYX_PUBLIC_KEY`.
    pub telnyx_public_key: Option<String>,
    /// How far a Telnyx delivery's `telnyx-timestamp` may be from THIS host's clock before the
    /// receiver refuses it even though its Ed25519 signature verified (kanban t_0e4ae7b7, the
    /// freshness arm). An absolute difference, so a stamp in the FUTURE is bounded the same way as
    /// one in the past. Defaults to Telnyx's own 300 s; a host whose clock wanders can be widened
    /// without a rebuild. `TELNYX_SIGNATURE_TOLERANCE_SECS`.
    pub telnyx_signature_tolerance_secs: i64,
    /// How far a Stripe delivery's `t=` stamp may be from THIS host's clock before the receiver
    /// refuses it even though its HMAC verified (kanban t_4754e612, the freshness arm of the
    /// `stripe_webhook` contract, the port of ADASwift t_08628ca6 / WorkflowSwift t_72a4bcdf). An
    /// absolute difference, so a stamp in the FUTURE is bounded the same way as one in the past.
    /// Defaults to Stripe's own 300 s; a host whose clock wanders can be widened without a
    /// rebuild. `STRIPE_WEBHOOK_TOLERANCE_SECS`.
    pub stripe_signature_tolerance_secs: i64,
    /// How long a request BODY may take to arrive before the request is answered `408` and its
    /// task, connection and partially-read body buffer are released (kanban t_7f688018, the
    /// missedcallrespondr arm of the fleet-wide body-read deadline).
    ///
    /// A bound on the body's ARRIVAL, not on the handler: these routes verify a signature or a key
    /// *after* the body is read, so a wall-clock budget over the handler would drop work that was
    /// legitimately in progress (a verified payment, a captured call). Same posture as the two
    /// bounds above: unset or unparseable falls back to the default rather than refusing to boot,
    /// and the value is clamped (5..=300) so a mistyped one cannot become an outage. The clamp and
    /// the default live in `body_deadline.rs`; `BODY_READ_DEADLINE_SECS` overrides.
    pub body_read_deadline_secs: u64,
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
            // `TAG_PROVISION_TENANT_SLUG` went with `POST /api/v1/internal/tag-provision` (kanban
            // t_c2353c90): the route had no caller, so nothing reads the slug any more.
            // Optional, empty when unset: see the field's doc comment. Read once here so the
            // webhook receiver never has to touch the environment per request.
            paypal_webhook_id: std::env::var("PAYPAL_WEBHOOK_ID").unwrap_or_default(),
            // Telnyx delivery verification (kanban t_0e4ae7b7). `Option`, and an empty value is
            // normalised to `None`, so a stray `TELNYX_PUBLIC_KEY=` line in the deploy env leaves
            // the receiver unconfigured (503, nothing applied) rather than "configured" with a key
            // that verifies nothing. Read once here so the request path never touches the env.
            telnyx_public_key: std::env::var("TELNYX_PUBLIC_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            // Telnyx signature freshness. Same posture as the bounds below: unset or unparseable
            // falls back to Telnyx's own 300 s rather than refusing to boot, and the value is
            // clamped so a mistyped one cannot become an outage — 0 would refuse every delivery
            // whose stamp is not this exact second, and a day-sized value would hand a captured
            // delivery a day-long replay window.
            telnyx_signature_tolerance_secs: std::env::var("TELNYX_SIGNATURE_TOLERANCE_SECS")
                .ok()
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(crate::security::telnyx_signature::DEFAULT_SIGNATURE_TOLERANCE_SECS)
                .clamp(30, 86_400),
            // Stripe signature freshness (kanban t_4754e612). Same posture as the two bounds
            // above: unset or unparseable falls back to the default (Stripe's own 300 s) rather
            // than refusing to boot, and the value is clamped so a mistyped one cannot become an
            // outage — 0 would refuse every delivery whose stamp is not this exact second, and a
            // day-sized value would hand a captured `Stripe-Signature` header a day-long replay
            // window.
            stripe_signature_tolerance_secs: std::env::var("STRIPE_WEBHOOK_TOLERANCE_SECS")
                .ok()
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(
                    crate::handlers::checkout_handler::DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS,
                )
                .clamp(30, 86_400),
            // Body-read deadline (kanban t_7f688018). 5 is the floor because below it the bound
            // starts shedding a legitimately slow sender; 300 the ceiling because above it the
            // bound stops being one for a stranger holding a connection open on a public receiver.
            body_read_deadline_secs: std::env::var("BODY_READ_DEADLINE_SECS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(crate::body_deadline::DEFAULT_BODY_READ_DEADLINE_SECS)
                .clamp(
                    crate::body_deadline::MIN_BODY_READ_DEADLINE_SECS,
                    crate::body_deadline::MAX_BODY_READ_DEADLINE_SECS,
                ),
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
    /// OPTIONAL: the signup page collects NAME + EMAIL only (David's model), so a missing
    /// `password` deserialises to `""`; the server then mints one and emails it.
    #[serde(default)]
    pub password: String,
    pub name: String,
    /// OPTIONAL: with no workspace-name field the handler derives `"<name>'s Workspace"`.
    #[serde(default)]
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

/// The signed-in account as `GET /api/v1/auth/me` answers it (programme card t_2cb77960, this app's
/// card t_9cd2c8f2).
///
/// Deliberately a SEPARATE struct from [`TeamMemberResponse`]: this one carries the fields the
/// account screen renders, including the REAL plan tier (`plan_name`, resolved from
/// `tenant_plans JOIN plans`) and the picture URL, neither of which the login/register responses
/// need or can cheaply know. The console signs in against `/auth/login` and then re-reads this
/// route, so `plan_name` is always the live tier and never a word the UI guessed.
///
/// Every optional field is `skip_serializing_if` so a field the account has not set is simply
/// absent from the JSON, and the console treats absent and null identically.
#[derive(Debug, Serialize, Deserialize)]
pub struct MeResponse {
    pub id: uuid::Uuid,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(rename = "account_id")]
    pub tenant_id: uuid::Uuid,
    pub role: String,
    /// The real plan tier name for this account's tenant ("Free", "Pro", …). Never the literal
    /// word "User" — that stray label is the FunnelSwift defect this programme removes.
    pub plan_name: String,
    /// `/api/v1/auth/avatar/<id>` when the account has uploaded a picture, absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
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
