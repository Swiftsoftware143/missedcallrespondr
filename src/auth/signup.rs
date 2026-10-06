//! ONE writer for a self-serve account (design §3.1 rule 4, kanban t_1d08bd9a).
//!
//! Both doors that create an account — the public `POST /api/v1/auth/register` and the
//! fleet-internal `POST /api/v1/internal/provision-free-account` (FunnelSwift tag → free account) —
//! call [`create_account`]. Sharing ONE implementation is what makes the account a tag mints the
//! SAME shape the marketing signup mints: its own `tenants` row, an `account_owner` `users` row with
//! a password, and a real `tenant_plans` row on the in-app plan carrying the free tier's 50 starter
//! credits. That shape is what makes the minted account log-in-able and **upgradeable in place**:
//! the `tenant_plans` row is what `features::{plan_slug, resolve_limit, resolve_flag}` read, and the
//! `users` row is what `auth::handlers::login` authenticates.
//!
//! Before this module `auth::handlers::register` was the only writer of that shape. The deleted
//! `/api/v1/internal/tag-provision` handler built its own inserts against a hardcoded tenant id
//! (kanban t_c9669881) — the duplication the design forbids, and the reason the old handler could
//! file a lead into a tenant nobody could log into. [`create_account`] mints its OWN tenant and keys
//! everything off the caller's contact address, so no request can name a tenant to write into.
//!
//! A THIRD minting site still exists and is deliberately untouched here: the paid-checkout path
//! `handlers::checkout_handler::deliver_credentials` mints a tenant + user when a payment completes
//! and seats no plan row. Folding it in would change the paid path's observable behaviour (its
//! `purchase_confirmed` mail, its existing-user arms) and is not this card; it is named in the audit
//! report instead of silently left out.

use chrono::Utc;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// Everything [`create_account`] needs.
///
/// `email` MUST already be normalised (`crate::security::email_addr::normalize`) — this function
/// does not re-normalise, so the caller owns the refusal for a malformed address (design §3.1
/// rule 5), exactly as `register` did when it owned the whole mint.
pub struct NewAccount<'a> {
    /// The login identity. Normalised, validated, and the idempotency key of both doors.
    pub email: &'a str,
    /// The owner user's display name.
    pub name: &'a str,
    /// An Argon2 hash — the caller hashes (so the plaintext never enters this module).
    pub password_hash: &'a str,
    /// The plaintext password, used ONLY for the `welcome_credentials` template's `{password}`
    /// placeholder. `None` keeps the public signup's behaviour: the user chose that password two
    /// seconds earlier, so the `welcome` mail carries no credential line — emailing a user-chosen
    /// secret only spreads it (card t_46d8d40e).
    pub password_plain: Option<&'a str>,
    /// The workspace label (`tenants.name`).
    pub account_name: &'a str,
    /// The workspace slug (`tenants.slug`, UNIQUE). `None` derives it from `account_name` the way
    /// `register` always has (`lower().replace(' ', "_")`); a caller that passes one owns the
    /// uniqueness of that value (the tag door does — two leads of one company would otherwise
    /// collide on this UNIQUE constraint and 500 the caller).
    pub account_slug: Option<&'a str>,
    /// The plan to seat, resolved IN-APP by the caller (design §3.1 rule 1). `plans.slug` is this
    /// app's plan identity; `register` passes `"free"`.
    pub plan_slug: &'a str,
    /// The owner user's role. `register` mints `account_owner`.
    pub role: &'a str,
}

/// The rows that were written. `account_id` is this app's notion of "the account" (`tenants.id`).
pub struct NewAccountIds {
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub account_name: String,
    pub account_slug: String,
}

/// Is this address already a login in this app? The ONE idempotency rule of every account door
/// (design §3.1 rule 3): `LOWER(email)`, case-insensitive so a row written before normalisation
/// existed still collides.
///
/// Exposed separately so a caller that hashes a password first (`register`, whose Argon2 work is
/// reachable without a credential and must not be spent on a duplicate) can refuse BEFORE hashing,
/// while [`create_account`] still checks again before its first write.
pub async fn email_taken(db: &sqlx::PgPool, email: &str) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE lower(email) = $1")
        .bind(email)
        .fetch_one(db)
        .await?;
    Ok(n > 0)
}

/// Create an account. Refuses (without writing anything) if the address is already a login.
pub async fn create_account(
    state: &AppState,
    a: NewAccount<'_>,
) -> Result<NewAccountIds, AppError> {
    let db = &state.pool;

    // The duplicate check runs BEFORE the first INSERT (the public signup's order, unchanged): an
    // orphan workspace must never be left behind by a refused retry.
    if email_taken(db, a.email).await? {
        return Err(AppError::Conflict(
            "A user with this email already exists. Try signing in.".into(),
        ));
    }

    let account_id = Uuid::new_v4();
    let account_slug = a
        .account_slug
        .map(str::to_string)
        .unwrap_or_else(|| a.account_name.to_lowercase().replace(' ', "_"));

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(account_id)
        .bind(a.account_name)
        .bind(&account_slug)
        .execute(db)
        .await?;

    let user_id = Uuid::new_v4();
    let now = Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(user_id)
    .bind(a.email)
    .bind(a.password_hash)
    .bind(a.name)
    .bind(account_id)
    .bind(a.role)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;

    // Seat the entry plan with its 50 starter credits. `register`'s statement and values verbatim;
    // only the slug is now a parameter instead of the literal `'free'`.
    let plan = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM plans WHERE slug = $1 AND is_active = true LIMIT 1",
    )
    .bind(a.plan_slug)
    .fetch_optional(db)
    .await?;

    if let Some((plan_id,)) = plan {
        sqlx::query(
            r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, credit_balance, lifetime_credits, status, billing_cycle)
               VALUES ($1, $2, $3, 50, 50, 'active', 'free')"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(plan_id)
        .execute(db)
        .await?;
    }

    // ── The welcome / credentials mail ───────────────────────────────────────────────────────────
    // Two shapes, one per door, both best-effort (a mail failure never fails the account):
    //   * a GENERATED password (the tag door): the mail is the only delivery of that password, so
    //     it is sent BEFORE this function returns and its failure is logged on the request's own
    //     line — the same posture `checkout_handler::deliver_credentials` already has for the
    //     password it generates, and the reason that path is awaited too.
    //   * a user-CHOSEN password (the public signup): `welcome`, no password placeholder, spawned
    //     exactly as before so the signup response never waits on the mail round-trip.
    if let Some(pw) = a.password_plain {
        let vars = serde_json::json!({
            "name": a.name,
            "email": a.email,
            "password": pw,
        });
        if let Err(e) =
            crate::email::send_template_email(db, account_id, a.email, "welcome_credentials", &vars)
                .await
        {
            tracing::error!(
                "account {} created but the CREDENTIALS EMAIL FAILED for {} — the customer has no password: {}",
                account_id,
                a.email,
                e
            );
        }
    } else {
        let pool = state.pool.clone();
        let email = a.email.to_string();
        let name = a.name.to_string();
        let account = account_id;
        tokio::spawn(async move {
            let vars = serde_json::json!({
                "name": name,
                "email": email,
                "app_name": "MissedCall Respondr",
                "login_url": "https://app.missedcallrespondr.com"
            });
            if let Err(e) =
                crate::email::send_template_email(&pool, account, &email, "welcome", &vars).await
            {
                // `error!`, not `warn!`: the account exists either way, so the ONLY signal that the
                // customer got no mail is this line (kanban t_6d575da6).
                tracing::error!(
                    "account created but the WELCOME EMAIL FAILED for {} — the customer has no welcome/credentials mail: {}",
                    email,
                    e
                );
            }
        });
    }

    Ok(NewAccountIds {
        account_id,
        user_id,
        account_name: a.account_name.to_string(),
        account_slug,
    })
}
