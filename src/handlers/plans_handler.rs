use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;
use crate::validation::{check_len, max};

#[derive(Debug, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct Plan {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub price_monthly: f64,
    pub price_yearly: f64,
    pub features: Option<serde_json::Value>,
    pub is_active: bool,
    pub sort_order: i32,
    pub payment_provider: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// Decode helpers
//
// `plans.created_at` / `updated_at` are `timestamp without time zone` and
// `price_monthly` / `price_yearly` are `numeric`. Decoding the timestamps as
// `DateTime<Utc>` and the numerics as `f64` fails in sqlx, and every call site
// used to swallow that failure (`try_get(..).ok()` / `.unwrap_or(0.0)`), so the
// admin plans table rendered NULL dates and $0.00 prices for four live rows with
// no log line and no 500. The types below match the columns; a decode failure is
// now logged instead of collapsing into the chrono/serde default.
// ---------------------------------------------------------------------------

/// A `timestamp without time zone` column, emitted as RFC3339 UTC. This app
/// writes these with `NOW()` from a UTC session, so the naive value is UTC.
fn ts_utc(row: &sqlx::postgres::PgRow, col: &str) -> Option<String> {
    use sqlx::Row;
    match row.try_get::<Option<NaiveDateTime>, _>(col) {
        Ok(v) => v.map(|d| d.and_utc().to_rfc3339()),
        Err(e) => {
            tracing::warn!(column = col, error = %e, "plans: timestamp decode failed");
            None
        }
    }
}

/// A `numeric` money column. sqlx has no `f64` decode for NUMERIC, so every
/// SELECT below casts the column to `float8` — the same convention this file
/// already uses in `attribute_plan_upgrade`. If that cast is ever dropped, this
/// says so instead of reporting $0.00 for a paid plan.
fn money(row: &sqlx::postgres::PgRow, col: &str) -> f64 {
    use sqlx::Row;
    match row.try_get::<Option<f64>, _>(col) {
        Ok(Some(v)) => v,
        Ok(None) => 0.0,
        Err(e) => {
            tracing::warn!(
                column = col,
                error = %e,
                "plans: numeric decode failed — the SELECT must cast this column to float8"
            );
            0.0
        }
    }
}

pub async fn list_plans(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    plans_payload(state, false).await
}

/// The TENANT-facing catalogue: `GET /api/v1/plans`.
///
/// WHY THIS IS A SEPARATE HANDLER (measured live 2026-10-18, cron `default`):
/// this route shared `list_plans` with the operator's `/api/v1/admin/plans`, so a signed-in
/// customer's own catalogue carried RETIRED plans. A fresh tenant's `GET /api/v1/plans` answered
/// four tiers, one of them `Pro ($49, is_active=false)` sitting next to the live
/// `Pro Monthly ($49)` — the "two $49 tiles" defect David reported, seen at the API rather than
/// only in the console. Each dashboard hid it with a client-side `.filter(p => p.is_active !==
/// false)`, which is a band-aid: every other client of the payload (the mobile app, any new
/// screen, any integration) shows the duplicate. The operator DOES need inactive rows (its Plans
/// table renders an Active/Inactive badge and can re-activate one), so the admin route keeps
/// `list_plans` and only this one filters.
pub async fn list_active_plans(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    plans_payload(state, true).await
}

async fn plans_payload(
    state: AppState,
    only_active: bool,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    // NOTE: price_monthly/price_yearly are `numeric`; the ::float8 cast is what
    // makes them decodable as f64 (see `money`).
    //
    // Ordered as a PRICE LADDER (kanban, cron `default`, 2026-10-13). `ORDER BY sort_order ASC,
    // price_monthly ASC` put the FREE tier BELOW every paid one, because `sort_order` was seeded
    // inconsistently (Enterprise/Pro = 0, Free = 1, Pro Monthly = 2): the operator's Plans table
    // rendered [Pro $49, Enterprise $199, Free $0, Pro Monthly $49]. Cheapest-first is the fleet
    // convention for a plan catalogue (ADASwift: `ORDER BY price ASC`), so the ladder is driven by
    // the price and ties break on the name. Same defect class as IncentiveSwift d5aed10b.
    // One catalogue query, two visibility rules: the tenant arm appends `WHERE is_active = true`
    // so a retired tier never reaches a buyer; the operator arm keeps every row.
    let sql = if only_active {
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, sort_order, payment_provider, \
         purchase_url, created_at, updated_at FROM plans WHERE is_active = true \
         ORDER BY price_monthly ASC NULLS LAST, name ASC"
    } else {
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, sort_order, payment_provider, \
         purchase_url, created_at, updated_at FROM plans ORDER BY price_monthly ASC NULLS LAST, name ASC"
    };
    let rows = sqlx::query(sql).fetch_all(&state.pool).await?;

    let plans: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "slug": r.try_get::<String,_>("slug").unwrap_or_default(),
            "description": r.try_get::<Option<String>,_>("description").ok().flatten(),
            "price_monthly": money(r, "price_monthly"),
            "price_yearly": money(r, "price_yearly"),
            "features": r.try_get::<Option<serde_json::Value>,_>("features").ok().flatten(),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(true),
            "sort_order": r.try_get::<i32, _>("sort_order").unwrap_or(0),
            "payment_provider": r.try_get::<Option<String>,_>("payment_provider").ok().flatten(),
            // The hosted payment page the OPERATOR pasted into the plan (2026-10-24). Read on the
            // catalogue so the operator's own console can show that a plan carries a link; the
            // buyer never needs it in the browser, because `POST /checkout/create` hands them to it
            // server-side (handlers::checkout_handler::create_checkout_session).
            "purchase_url": r.try_get::<Option<String>,_>("purchase_url").ok().flatten(),
            "created_at": ts_utc(r, "created_at"),
            "updated_at": ts_utc(r, "updated_at"),
        })
    }).collect();

    Ok(Json(json!({"plans": plans, "total": plans.len()})))
}

pub async fn get_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, sort_order, payment_provider, \
         purchase_url, created_at, updated_at FROM plans WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".into()))?;

    Ok(Json(json!({"plan": {
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "slug": row.try_get::<String,_>("slug").unwrap_or_default(),
        "description": row.try_get::<Option<String>,_>("description").ok().flatten(),
        "price_monthly": money(&row, "price_monthly"),
        "price_yearly": money(&row, "price_yearly"),
        "features": row.try_get::<Option<serde_json::Value>,_>("features").ok().flatten(),
        "is_active": row.try_get::<bool,_>("is_active").unwrap_or(true),
        "sort_order": row.try_get::<i32,_>("sort_order").unwrap_or(0),
        "payment_provider": row.try_get::<Option<String>,_>("payment_provider").ok().flatten(),
        "purchase_url": row.try_get::<Option<String>,_>("purchase_url").ok().flatten(),
        "created_at": ts_utc(&row, "created_at"),
        "updated_at": ts_utc(&row, "updated_at"),
    }})))
}

/// A plan's `purchase_url` is a hosted payment page the OPERATOR types in, so refuse a typo at
/// SAVE time instead of at the till.
///
/// The till already drops an unusable link (`checkout_handler::is_hosted_payment_url`) — but the
/// operator only found out when a customer pressed Buy: measured live 2026-10-25,
/// `POST /api/v1/admin/plans` stored `buy.stripe.com/not-a-url` and answered "Plan created", after
/// which every press on Upgrade fell through to the refusal arm on a plan the operator believed was
/// sellable. A blank box stays allowed (a free plan, or one sold another way, carries no link).
fn require_hosted_payment_url(raw: &str) -> Result<(), AppError> {
    let u = raw.trim();
    if u.is_empty() || crate::handlers::checkout_handler::is_hosted_payment_url(u) {
        return Ok(());
    }
    Err(AppError::BadRequest(
        "The payment link must be a full https:// address, for example https://buy.stripe.com/abc123. \
         It is where a buyer is sent when they pick this plan, so a link without https:// cannot take \
         a payment. Leave the box empty if this plan is not sold by link."
            .to_string(),
    ))
}

pub async fn create_plan(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let id = Uuid::new_v4();
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let price_monthly = req
        .get("price_monthly")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let price_yearly = req
        .get("price_yearly")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let features = req.get("features");
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if name.is_empty() {
        return Err(AppError::BadRequest("Plan name is required".into()));
    }

    let payment_provider = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // ── A PLAN CAN CARRY ITS OWN PAYMENT PAGE (2026-10-24) ──────────────────────────────────
    // `plans.purchase_url` has existed since the plans table shipped and NO code path in this app
    // could write or read it: the admin form had no box for it and the till never looked at the
    // column, so on an install whose `payment_providers` is EMPTY (this one) every press on Upgrade
    // could only be refused even when the operator already had a working way to be paid. An
    // operator who already has one — a Stripe Payment Link, a PayPal button, any hosted page — can
    // now paste it here and the till hands the buyer straight to it.
    // `checkout_handler::is_hosted_payment_url` judges a typo at the till, so a bad value cannot
    // send a buyer somewhere broken. Blank means "no link".
    let purchase_url = req
        .get("purchase_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if let Some(url) = purchase_url.as_deref() {
        require_hosted_payment_url(url)?;
    }

    sqlx::query(
        r#"INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, is_active, payment_provider, purchase_url)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#
    )
    .bind(id)
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(features)
    .bind(is_active)
    .bind(&payment_provider)
    .bind(&purchase_url)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "message": "Plan created"})),
    ))
}

pub async fn update_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    // `payment_provider` MUST be selected: the arms below read it (line ~269) and the value is
    // WRITTEN BACK by the UPDATE, so omitting it here turned every save that did not carry the key
    // into `SET payment_provider = NULL` over live data (kanban t_8b053bb3). The `try_get().ok()`
    // there never panics — it just hides the drift and hands the UPDATE a None.
    let existing = sqlx::query(
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, payment_provider, sort_order, \
         purchase_url FROM plans WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".into()))?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = if name.is_empty() {
        existing.try_get::<String, _>("name").unwrap_or_default()
    } else {
        name
    };
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| existing.try_get::<String, _>("slug").unwrap_or_default());
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let description: Option<String> = description.or_else(|| {
        existing
            .try_get::<Option<String>, _>("description")
            .ok()
            .flatten()
    });
    let price_monthly = req
        .get("price_monthly")
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| money(&existing, "price_monthly"));
    let price_yearly = req
        .get("price_yearly")
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| money(&existing, "price_yearly"));
    let features = req.get("features").cloned().or_else(|| {
        existing
            .try_get::<Option<serde_json::Value>, _>("features")
            .ok()
            .flatten()
    });
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| existing.try_get::<bool, _>("is_active").unwrap_or(true));
    let payment_provider: Option<String> = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            existing
                .try_get::<Option<String>, _>("payment_provider")
                .ok()
                .flatten()
        });

    // The plan's own hosted payment page. THREE cases, and they must stay distinguishable:
    // an ABSENT field leaves whatever is stored (so an unrelated save cannot wipe a link); a JSON
    // `null` or an empty box CLEARS it (that is how the operator takes a plan off sale-by-link);
    // anything else sets it. A plain `Option<String>` read of the request would collapse "absent"
    // and "null" into one value, which is why the `None`/`Some` arm is spelled out here rather than
    // folded into the `.or_else(existing…)` idiom the other columns use.
    let purchase_url: Option<String> = match req.get("purchase_url") {
        None => existing
            .try_get::<Option<String>, _>("purchase_url")
            .ok()
            .flatten(),
        Some(v) => match v.as_str() {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    require_hosted_payment_url(trimmed)?;
                    Some(trimmed.to_string())
                }
            }
            // A JSON `null` clears the link, never leaves it.
            None => None,
        },
    };

    sqlx::query(
        r#"UPDATE plans SET name=$1, slug=$2, description=$3, price_monthly=$4, price_yearly=$5,
           features=$6, is_active=$7, payment_provider=$8, purchase_url=$9 WHERE id=$10"#,
    )
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(&features)
    .bind(is_active)
    .bind(&payment_provider)
    .bind(&purchase_url)
    .bind(id)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({"message": "Plan updated"})))
}

pub async fn delete_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM plans WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Plan not found".into()));
    }

    Ok(Json(json!({"message": "Plan deleted"})))
}

pub async fn admin_update_plan_features(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let features = req
        .get("features")
        .ok_or_else(|| AppError::BadRequest("features object required".into()))?;

    // Shape guard (kanban t_dd2f7e32). This action MERGES jsonb (`features || $1`), and Postgres
    // concatenates array-with-array or object-with-object — so merging an object into a plan whose
    // `features` is an ARRAY of marketing tags (enterprise, pro live that way) APPLIES the object as
    // an extra array ELEMENT: the request answers 200, the panel shows a save, and `features->>'key'`
    // still resolves NULL — i.e. a grant that grants nothing, with no error anywhere. Refuse the
    // mixed write instead of storing a value no gate can read.
    let current: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT features FROM plans WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
    let current = current.ok_or_else(|| AppError::NotFound("Plan not found".into()))?;
    if current.is_array() != features.is_array() {
        return Err(AppError::BadRequest(format!(
            "plans.features on this plan is a JSON {}; merging a JSON {} into it would append an \
             element no gate can read (silently doing nothing). Grant a registry key with the panel's \
             `Set plan feature` control (PUT /api/v1/admin/plans/entitlement) instead.",
            if current.is_array() { "array" } else { "object" },
            if features.is_array() { "array" } else { "object" }
        )));
    }

    let features_str = features.to_string();
    sqlx::query(
        "UPDATE plans SET features = COALESCE(features::text, '{}')::jsonb || $1::jsonb, updated_at=NOW() WHERE id=$2"
    )
    .bind(&features_str)
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({"message": "Features updated"})))
}

/// THE ONE rule for "which plan is the TOP tier" (kanban t_b73c597e).
///
/// The catalogue's own ORDER BY used to *be* the definition: `sort_order ASC, price_monthly DESC`
/// made `plan_registry` and `grant_top_tier` agree only by accident, because `sort_order` carries no
/// intended meaning — it defaults to 0 and only the SEEDED rows set it (migration 000008 puts Free at
/// 1 and Pro Monthly at 2), so Enterprise and Pro both sat at 0 and the winner hung on a tie-break.
/// The top tier is now the most expensive ACTIVE plan, and BOTH surfaces (the registry panel and the
/// `Grant top tier every feature` button) read it from here, so the two cannot disagree.
pub async fn top_plan(db: &PgPool) -> Result<(Uuid, String), AppError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, slug FROM plans WHERE is_active \
         ORDER BY price_monthly DESC NULLS LAST, price_yearly DESC NULLS LAST, slug ASC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    row.ok_or_else(|| AppError::NotFound("No active plan is defined".into()))
}

/// Seat THIS app's highest ACTIVE plan on a portfolio tenant.
///
/// David's standing rule (2026-10-04): every portfolio tenant is a sister company he owns, so it
/// always gets the highest level of each software. Measured live 2026-10-17: NEITHER portfolio door
/// wrote a `tenant_plans` row at all — the fleet's own pushed company "Giraudy Capital" sat on no
/// tier — so a sister company arrived with only the free defaults while the operator believed it
/// held the top plan, and every feature gate read that default.
///
/// The write itself is `seat_plan_for_tenant` (the one seat every other door uses). Best-effort ON
/// PURPOSE: the account being provisioned is the deliverable, so an empty catalogue must never fail
/// a provision.
pub async fn seat_top_plan_on(state: &AppState, tenant_id: Uuid) -> Option<String> {
    match top_plan(&state.pool).await {
        Ok((plan_id, slug)) => {
            match seat_plan_for_tenant(state, tenant_id, plan_id, Some("manual")).await {
                Ok(()) => {
                    tracing::info!(tenant = %tenant_id, plan = %slug,
                        "portfolio tenant seated on the top plan");
                    Some(slug)
                }
                Err(e) => {
                    tracing::warn!(tenant = %tenant_id, error = ?e,
                        "portfolio tenant: seating the top plan failed");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(tenant = %tenant_id, error = ?e,
                "portfolio tenant: no active plan exists to seat");
            None
        }
    }
}

/// GET /api/v1/admin/plan-registry — the plan × feature CATALOGUE (kanban t_dd2f7e32).
///
/// One row per registry key, one column per live plan, plus the resolved value and its SOURCE for
/// every plan × key. Both halves come from `crate::features::{resolve_limit, resolve_flag}` — the
/// very functions the gates call — so "what the panel shows" is, by construction, the trade the
/// gate enforced (the card's requirement 4: gate == panel).
///
/// `top_plan` comes from `top_plan()` above (the most expensive ACTIVE plan) — not from the plan's
/// name, and no longer from the display order.
///
/// The columns themselves are a PRICE LADDER, cheapest-first (kanban t_b73c597e): the panel used to
/// be ordered `sort_order ASC, price_monthly DESC`, which put the FREE tier ($0) BELOW both paid
/// tiers and the $49 `Pro Monthly` last — the operator read the catalogue in an order no price
/// ladder can produce. Same rule as `list_plans` and the fleet convention (ADASwift: `ORDER BY
/// price ASC`).
pub async fn plan_registry(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, name, slug, is_active, sort_order, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, price::float8 AS price, purchase_url FROM plans \
         ORDER BY is_active DESC, price_monthly ASC NULLS LAST, name ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut plan_rows: Vec<(Uuid, String, String, bool, i32, f64, f64)> = Vec::new();
    // The panel's plan form is seeded from THIS payload, so the operator's own hosted payment page
    // has to travel with it — otherwise the edit modal opens blank and saving would clear the link.
    let mut purchase_urls: std::collections::HashMap<Uuid, Option<String>> = Default::default();
    for r in &rows {
        purchase_urls.insert(
            r.try_get::<Uuid, _>("id").unwrap_or_default(),
            r.try_get::<Option<String>, _>("purchase_url")
                .ok()
                .flatten(),
        );
        plan_rows.push((
            r.try_get::<Uuid, _>("id").unwrap_or_default(),
            r.try_get::<String, _>("slug").unwrap_or_default(),
            r.try_get::<String, _>("name").unwrap_or_default(),
            r.try_get::<bool, _>("is_active").unwrap_or(true),
            r.try_get::<i32, _>("sort_order").unwrap_or(0),
            money(r, "price_monthly"),
            money(r, "price_yearly"),
        ));
    }
    // The top tier is NOT the first display row (the ladder is cheapest-first); it is the most
    // expensive ACTIVE plan, from the one shared rule so this panel and `grant_top_tier` agree.
    let top_slug = top_plan(&state.pool).await?.1;

    // Resolve every registry key for every plan, remembering value + source per kind.
    let mut limit_vals: std::collections::HashMap<(String, String), Option<i64>> =
        Default::default();
    let mut limit_src: std::collections::HashMap<(String, String), &'static str> =
        Default::default();
    let mut flag_vals: std::collections::HashMap<(String, String), Option<bool>> =
        Default::default();
    let mut flag_src: std::collections::HashMap<(String, String), &'static str> =
        Default::default();
    let mut matrix: Vec<serde_json::Value> = Vec::new();

    for plan in &plan_rows {
        for def in crate::feature_registry::REGISTRY {
            match def.kind {
                crate::feature_registry::FeatureKind::Limit => {
                    let (v, src) =
                        crate::features::resolve_limit(&state.pool, &plan.1, def.key).await?;
                    limit_vals.insert((plan.1.clone(), def.key.to_string()), v);
                    limit_src.insert((plan.1.clone(), def.key.to_string()), src.as_str());
                }
                crate::feature_registry::FeatureKind::Boolean => {
                    let (v, src) =
                        crate::features::resolve_flag(&state.pool, &plan.1, def.key).await?;
                    flag_vals.insert((plan.1.clone(), def.key.to_string()), v);
                    flag_src.insert((plan.1.clone(), def.key.to_string()), src.as_str());
                }
            }
        }
    }

    // The superset claim: no OTHER plan may beat the top tier on any registry key. Asserting it is
    // the whole point of the card — "it has rows now" is not the property.
    let mut violations: Vec<String> = Vec::new();
    for def in crate::feature_registry::REGISTRY {
        for plan in &plan_rows {
            if plan.1 == top_slug {
                continue;
            }
            match def.kind {
                crate::feature_registry::FeatureKind::Limit => {
                    let top = limit_vals
                        .get(&(top_slug.clone(), def.key.to_string()))
                        .copied()
                        .flatten();
                    let other = limit_vals
                        .get(&(plan.1.clone(), def.key.to_string()))
                        .copied()
                        .flatten();
                    let top_unlimited = top.map(|v| v < 0).unwrap_or(true);
                    if top_unlimited {
                        continue;
                    }
                    let top_cap = top.unwrap_or(0);
                    let beats = match other {
                        // unset ⇒ the absence rule ALLOWS, i.e. effectively unlimited ⇒ beats a cap
                        None => true,
                        Some(v) if v < 0 => true,
                        Some(v) => v > top_cap,
                    };
                    if beats {
                        violations.push(format!(
                            "{}: {} resolves {:?} while the top tier ({}) is capped at {}",
                            def.key, plan.1, other, top_slug, top_cap
                        ));
                    }
                }
                crate::feature_registry::FeatureKind::Boolean => {
                    let top = flag_vals
                        .get(&(top_slug.clone(), def.key.to_string()))
                        .copied()
                        .flatten();
                    let other = flag_vals
                        .get(&(plan.1.clone(), def.key.to_string()))
                        .copied()
                        .flatten();
                    if top != Some(true) && other == Some(true) {
                        violations.push(format!(
                            "{}: {} is granted while the top tier ({}) is not",
                            def.key, plan.1, top_slug
                        ));
                    }
                }
            }
        }
    }

    // The panel's table: one row per registry key, one column per live plan, flat strings.
    for def in crate::feature_registry::REGISTRY {
        let mut row = serde_json::Map::new();
        row.insert("feature".into(), json!(def.key));
        row.insert("what".into(), json!(def.label));
        row.insert(
            "kind".into(),
            json!(match def.kind {
                crate::feature_registry::FeatureKind::Limit => "limit",
                crate::feature_registry::FeatureKind::Boolean => "boolean",
            }),
        );
        row.insert("if_unset".into(), json!(def.unset_means()));
        row.insert("enforced_by".into(), json!(def.enforced_by));
        for plan in &plan_rows {
            let cell = match def.kind {
                crate::feature_registry::FeatureKind::Limit => {
                    match limit_vals
                        .get(&(plan.1.clone(), def.key.to_string()))
                        .copied()
                        .flatten()
                    {
                        Some(v) if v < 0 => format!("unlimited ({})", v),
                        Some(0) => "NOT available (0)".to_string(),
                        Some(v) => format!("cap {}", v),
                        None => "unset -> allowed".to_string(),
                    }
                }
                crate::feature_registry::FeatureKind::Boolean => {
                    match flag_vals
                        .get(&(plan.1.clone(), def.key.to_string()))
                        .copied()
                        .flatten()
                    {
                        Some(true) => "granted".to_string(),
                        Some(false) => "NOT granted".to_string(),
                        None => "unset -> refused".to_string(),
                    }
                }
            };
            row.insert(plan.1.clone(), json!(cell));
        }
        matrix.push(serde_json::Value::Object(row));
    }

    // Per-plan detail: the resolved value (typed) + the store it came from.
    let plans_json: Vec<serde_json::Value> = plan_rows
        .iter()
        .map(|p| {
            let mut values = serde_json::Map::new();
            let mut sources = serde_json::Map::new();
            for def in crate::feature_registry::REGISTRY {
                let k = (p.1.clone(), def.key.to_string());
                match def.kind {
                    crate::feature_registry::FeatureKind::Limit => {
                        values.insert(def.key.into(), json!(limit_vals.get(&k).copied().flatten()));
                        sources.insert(
                            def.key.into(),
                            json!(limit_src.get(&k).copied().unwrap_or("unset")),
                        );
                    }
                    crate::feature_registry::FeatureKind::Boolean => {
                        values.insert(def.key.into(), json!(flag_vals.get(&k).copied().flatten()));
                        sources.insert(
                            def.key.into(),
                            json!(flag_src.get(&k).copied().unwrap_or("unset")),
                        );
                    }
                }
            }
            json!({
                "id": p.0.to_string(),
                "slug": p.1,
                "name": p.2,
                "is_active": p.3,
                "sort_order": p.4,
                "price_monthly": p.5,
                "price_yearly": p.6,
                "purchase_url": purchase_urls.get(&p.0).cloned().flatten(),
                "is_top": p.1 == top_slug,
                "values": values,
                "sources": sources,
            })
        })
        .collect();

    let registry_json: Vec<serde_json::Value> = crate::feature_registry::REGISTRY
        .iter()
        .map(|d| {
            json!({
                "key": d.key,
                "label": d.label,
                "kind": match d.kind {
                    crate::feature_registry::FeatureKind::Limit => "limit",
                    crate::feature_registry::FeatureKind::Boolean => "boolean",
                },
                "unit": d.unit,
                "unset_means": d.unset_means(),
                "enforced_by": d.enforced_by,
                "storage": match d.storage {
                    crate::feature_registry::Storage::Column(c) => format!("plans.{c}"),
                    crate::feature_registry::Storage::FeatureLimits => "feature_limits".to_string(),
                },
                "read_by_gate": d.read_by_gate,
            })
        })
        .collect();

    Ok(Json(json!({
        "grant_matrix": matrix,
        "plans": plans_json,
        "registry": registry_json,
        "superset_ok": violations.is_empty(),
        "superset_violations": violations,
        "top_plan": top_slug,
        "top_rule": "the most expensive ACTIVE plan (price_monthly DESC, then price_yearly DESC, then slug ASC); the table itself lists plans cheapest-first",
        "value_legend": "-1 = unlimited/granted, 0 = NOT available on this plan, N>0 = a cap of N; booleans: non-zero = on, 0 = off",
    })))
}

/// PUT /api/v1/admin/plans/entitlement — the panel's "Set plan feature" control (kanban
/// t_dd2f7e32). Body: `{"plan": <slug or id>, "feature": <registry key>, "value": <int>}`.
///
/// The grant is only "manageable by David" if the panel can express it: this is the write path the
/// operator console calls, it validates the key against the registry (a typo can never be stored as
/// a dead plan key), and it answers with the RESOLVED value re-read through the gate's own resolver.
pub async fn set_plan_entitlement(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let plan_ref = req
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let feature = req
        .get("feature")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let value = req.get("value").and_then(|v| v.as_i64()).ok_or_else(|| {
        AppError::BadRequest(
            "`value` must be an integer (-1 unlimited, 0 not available, N a cap).".into(),
        )
    })?;

    let def = crate::feature_registry::find(&feature).ok_or_else(|| {
        AppError::BadRequest(format!(
            "Unknown feature key '{}'. Valid keys: {}",
            feature,
            crate::feature_registry::keys()
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    match def.kind {
        crate::feature_registry::FeatureKind::Boolean => {
            if !(-1..=1).contains(&value) {
                return Err(AppError::BadRequest(format!(
                    "'{}' is an on/off feature: send 1 (granted), 0 (not available) or -1 (granted/unlimited).",
                    feature
                )));
            }
        }
        crate::feature_registry::FeatureKind::Limit => {
            if !(-1..=1_000_000_000).contains(&value) {
                return Err(AppError::BadRequest(format!(
                    "'{}' takes -1 (unlimited), 0 (not available) or a positive cap.",
                    feature
                )));
            }
        }
    }

    if plan_ref.is_empty() {
        return Err(AppError::BadRequest(
            "`plan` is required (plan slug, e.g. `enterprise`).".into(),
        ));
    }
    // Slug first (what the panel's field takes), then an id, so both are usable.
    let plan_id: Option<Uuid> = match Uuid::parse_str(&plan_ref) {
        Ok(id) => sqlx::query_scalar("SELECT id FROM plans WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .flatten(),
        Err(_) => sqlx::query_scalar("SELECT id FROM plans WHERE slug = $1")
            .bind(&plan_ref)
            .fetch_optional(&state.pool)
            .await?
            .flatten(),
    };
    let plan_id =
        plan_id.ok_or_else(|| AppError::NotFound(format!("No plan matches '{plan_ref}'")))?;

    // An explicit `features->>key` override wins over a dedicated column (talisman of t_0e61628c):
    // say so instead of silently writing a column the gate will not read.
    let mut warning: Option<String> = None;
    match def.storage {
        crate::feature_registry::Storage::Column(col) => {
            let overridden: Option<String> =
                sqlx::query_scalar("SELECT features->>$1 FROM plans WHERE id = $2")
                    .bind(feature.as_str())
                    .bind(plan_id)
                    .fetch_optional(&state.pool)
                    .await?
                    .flatten();
            if overridden.is_some() {
                warning = Some(format!(
                    "this plan carries an explicit features.\"{feature}\" override, which resolves BEFORE plans.{col}; change it with the raw `Set plan features (JSON)` action"
                ));
            }
            // Compile-time literal per key — a query text built from a run-time string is the gate
            // 5d defect this fleet already fixed once (class 14).
            let sql = match col {
                "max_leads" => "UPDATE plans SET max_leads = $2, updated_at = NOW() WHERE id = $1",
                // max_tags joined the registry in t_b578b169; its column is INT4 like max_leads.
                // Without this arm the panel's own "Set plan feature" control answered 500 for the
                // key it advertised — a registry entry the panel can SHOW but not SET is not
                // manageable by the operator.
                "max_tags" => "UPDATE plans SET max_tags = $2, updated_at = NOW() WHERE id = $1",
                _ => {
                    return Err(AppError::Internal(format!(
                        "registry key '{feature}' names column '{col}' with no registered UPDATE"
                    )))
                }
            };
            sqlx::query(sql)
                .bind(plan_id)
                .bind(value as i32)
                .execute(&state.pool)
                .await?;
        }
        crate::feature_registry::Storage::FeatureLimits => {
            sqlx::query(
                "INSERT INTO feature_limits (plan_id, feature_key, limit_value) VALUES ($1, $2, $3) \
                 ON CONFLICT (plan_id, feature_key) DO UPDATE SET limit_value = EXCLUDED.limit_value",
            )
            .bind(plan_id)
            .bind(feature.as_str())
            .bind(value)
            .execute(&state.pool)
            .await?;
        }
    }

    let slug: String = sqlx::query_scalar("SELECT slug FROM plans WHERE id = $1")
        .bind(plan_id)
        .fetch_one(&state.pool)
        .await?;
    // Re-read through the GATE's resolver, so the answer is the trade the app will enforce.
    let (resolved, source) = match def.kind {
        crate::feature_registry::FeatureKind::Limit => {
            let (v, s) = crate::features::resolve_limit(&state.pool, &slug, &feature).await?;
            (json!(v), s.as_str())
        }
        crate::feature_registry::FeatureKind::Boolean => {
            let (v, s) = crate::features::resolve_flag(&state.pool, &slug, &feature).await?;
            (json!(v), s.as_str())
        }
    };

    Ok(Json(json!({
        "success": true,
        "plan": slug,
        "plan_id": plan_id.to_string(),
        "feature": feature,
        "value": value,
        "storage": match def.storage {
            crate::feature_registry::Storage::Column(c) => format!("plans.{c}"),
            crate::feature_registry::Storage::FeatureLimits => "feature_limits".to_string(),
        },
        "resolved": resolved,
        "resolved_from": source,
        "warning": warning,
    })))
}

/// POST /api/v1/admin/plans/grant-top-tier — re-apply David's standing directive ("the top tier
/// plan gets everything") from the panel, idempotently (kanban t_dd2f7e32).
///
/// GAP-FILLING ONLY: a key the top tier already grants is left EXACTLY as it is (a cap David set is
/// never raised to unlimited by pressing this button); only keys that resolve as unset are seated —
/// `-1` (unlimited) for a limit, `1` (granted) for an on/off feature. Running it twice changes
/// nothing the second time, so it is safe to press after adding a key or a plan.
pub async fn grant_top_tier(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The SAME rule the registry panel reports as `top_plan` (kanban t_b73c597e) — the button and
    // the table can no longer name different plans for "the top tier".
    let (plan_id, slug) = top_plan(&state.pool).await?;

    let mut granted: Vec<String> = Vec::new();
    let mut already: Vec<String> = Vec::new();
    for def in crate::feature_registry::REGISTRY {
        let unresolved = match def.kind {
            crate::feature_registry::FeatureKind::Limit => {
                crate::features::resolve_limit(&state.pool, &slug, def.key)
                    .await?
                    .0
                    .is_none()
            }
            crate::feature_registry::FeatureKind::Boolean => {
                crate::features::resolve_flag(&state.pool, &slug, def.key)
                    .await?
                    .0
                    .is_none()
            }
        };
        if !unresolved {
            already.push(def.key.to_string());
            continue;
        }
        let value: i64 = match def.kind {
            crate::feature_registry::FeatureKind::Limit => -1, // -1 = unlimited
            crate::feature_registry::FeatureKind::Boolean => 1, // 1 = granted
        };
        match def.storage {
            crate::feature_registry::Storage::Column(col) => {
                let sql = match col {
                    "max_leads" => {
                        "UPDATE plans SET max_leads = $2, updated_at = NOW() WHERE id = $1"
                    }
                    _ => {
                        return Err(AppError::Internal(format!(
                            "registry key '{}' names column '{col}' with no registered UPDATE",
                            def.key
                        )))
                    }
                };
                sqlx::query(sql)
                    .bind(plan_id)
                    .bind(value as i32)
                    .execute(&state.pool)
                    .await?;
            }
            crate::feature_registry::Storage::FeatureLimits => {
                sqlx::query(
                    "INSERT INTO feature_limits (plan_id, feature_key, limit_value) VALUES ($1, $2, $3) \
                     ON CONFLICT (plan_id, feature_key) DO UPDATE SET limit_value = EXCLUDED.limit_value",
                )
                .bind(plan_id)
                .bind(def.key)
                .bind(value)
                .execute(&state.pool)
                .await?;
            }
        }
        granted.push(def.key.to_string());
    }

    Ok(Json(json!({
        "success": true,
        "top_plan": slug,
        "granted": granted,
        "already_granted": already,
        "note": "gap-filling only: a key the top tier already grants was left untouched",
    })))
}

/// Fire-and-forget notification to FunnelSwift that a tenant upgraded to a paid plan,
/// so the referring affiliate is credited (permanent, no expiry).
async fn notify_funnelswift_upgrade(
    funnelswift_url: &str,
    internal_sync_key: &str,
    email: &str,
    plan_name: &str,
    plan_price: f64,
    event_id: &str,
) {
    if email.is_empty() || funnelswift_url.is_empty() {
        return;
    }
    let url = format!(
        "{}/api/v1/internal/affiliate/upgrade-event",
        funnelswift_url.trim_end_matches('/')
    );
    let key = internal_sync_key.to_string();
    let payload = serde_json::json!({
        "source_app": "missedcallrespondr",
        "email": email,
        "plan_name": plan_name,
        "plan_price": plan_price,
        "event_id": event_id,
    });
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(&url)
            .header("x-internal-key", key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    });
}

/// Resolve the tenant's owner email + plan, and notify FunnelSwift if it's a PAID upgrade.
async fn attribute_plan_upgrade(state: &AppState, tenant_id: Uuid, plan_id: Uuid) {
    let plan: Option<(String, Option<f64>)> =
        match sqlx::query_as("SELECT name, price_monthly::float8 FROM plans WHERE id = $1")
            .bind(plan_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(
                    "plans.attribute_plan_upgrade: plan lookup failed (plan_id={plan_id}): {e}"
                );
                return;
            }
        };
    let Some((plan_name, price)) = plan else {
        return;
    };
    let plan_price = price.unwrap_or(0.0);
    if plan_price <= 0.0 {
        return;
    }
    let email: Option<String> = match sqlx::query_scalar(
        "SELECT email FROM users WHERE tenant_id = $1 AND role IN ('admin','company_admin','account_owner') LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(
                "plans.attribute_plan_upgrade: owner email lookup failed (tenant_id={tenant_id}): {e}"
            );
            return;
        }
    };
    let Some(email) = email else {
        return;
    };
    notify_funnelswift_upgrade(
        &state.funnelswift_url,
        &state.config.internal_sync_key,
        &email,
        &plan_name,
        plan_price,
        &Uuid::new_v4().to_string(),
    )
    .await;
}

/// Seat `plan_id` as the ACTIVE plan on an EXISTING tenant — the "upgrade-shaped" seat, where the
/// tenant may already hold a `tenant_plans` row. ONE statement, two callers; `cycle` is the whole
/// difference between them (kanban t_3c97243d).
///
/// The invariant is one plan row per tenant (`tenant_plans_tenant_id_key`), so an upgrade can only be
/// an UPDATE of that row and an `ON CONFLICT (tenant_id)` upsert is the only correct shape — the
/// UPDATE arm touches neither `credit_balance` nor `lifetime_credits`, because re-seating a plan must
/// not reset a balance.
///
///   * `Some(c)` — the panel's *Assign plan* action (`admin_assign_plan`): the CALLER names the cycle
///     and it is written to both arms (the panel asked for exactly that field; the old statement
///     silently kept the previous cycle, a 2xx that did not change the field the caller asked for).
///   * `None` — the paid-checkout completion arm for a buyer who ALREADY has an account (kanban
///     t_3c97243d): **no cycle can be named, so none is manufactured.** The session carries no cycle
///     (`checkout_sessions` has no such column and its `metadata` is caller-shaped free JSON); this
///     app's `plans` rows price BOTH terms (`price_monthly` + `price_yearly`, no interval column), so
///     no value is derivable from what was bought either; and the doors that already write the column
///     DISAGREE (`'free'` from the signup seat, the caller's value from the panel, `'manual'` from the
///     billing panel). So the UPDATE arm leaves whatever the row already claims and the INSERT arm
///     takes the column's own DEFAULT (`'free'`). Recorded as NEEDS-DAVID: before a paid arm can
///     write a real term, the create path must carry it in the session.
pub(crate) async fn seat_plan_for_tenant(
    state: &AppState,
    tenant_id: Uuid,
    plan_id: Uuid,
    cycle: Option<&str>,
) -> Result<(), sqlx::Error> {
    match cycle {
        Some(cycle) => {
            sqlx::query(
                r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle)
                   VALUES ($1, $2, $3, 'active', $4)
                   ON CONFLICT (tenant_id) DO UPDATE SET plan_id=$3, status='active', billing_cycle=$4, updated_at=NOW()"#,
            )
            .bind(Uuid::new_v4())
            .bind(tenant_id)
            .bind(plan_id)
            .bind(cycle)
            .execute(&state.pool)
            .await?;
        }
        None => {
            sqlx::query(
                r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status)
                   VALUES ($1, $2, $3, 'active')
                   ON CONFLICT (tenant_id) DO UPDATE SET plan_id=$3, status='active', updated_at=NOW()"#,
            )
            .bind(Uuid::new_v4())
            .bind(tenant_id)
            .bind(plan_id)
            .execute(&state.pool)
            .await?;
        }
    }
    Ok(())
}

pub async fn admin_assign_plan(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let tenant_id = req
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("Valid tenant_id is required".into()))?;
    let plan_id = req
        .get("plan_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("Valid plan_id is required".into()))?;
    let billing_cycle = req
        .get("billing_cycle")
        .and_then(|v| v.as_str())
        .unwrap_or("monthly");
    // The panel sends this string straight into `tenant_plans.billing_cycle VARCHAR(50)`; before
    // kanban t_dd7be032 a 51-character cycle answered 500 after the plan had been re-assigned, i.e.
    // a refused request that still changed data. Checked before any statement.
    check_len(
        "billing_cycle",
        billing_cycle,
        max::TENANT_PLANS_BILLING_CYCLE,
    )?;

    // Validate the two foreign keys BEFORE the write (measured live 2026-10-04): the INSERT
    // below carries `tenant_plans_tenant_id_fkey` AND `tenant_plans_plan_id_fkey`, so a
    // non-existent tenant_id (or plan_id) aborted the statement and the panel's assign
    // action answered 500 {"error":"Database error"}. Both are caller errors — name them.
    let tenant_exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?;
    if tenant_exists.is_none() {
        return Err(AppError::NotFound(format!("No tenant with id {tenant_id}")));
    }
    // The plan must exist AND still be on sale. MEASURED LIVE 2026-10-20: this action accepted
    // the RETIRED `Pro` row (`is_active = false`, slug `pro`) — `POST /api/v1/admin/plans/assign`
    // answered 200, `tenant_plans` moved to it, and the customer's own `/auth/me` then reported
    // `plan_name: "Pro"` while their billing screen offered only Free / Pro Monthly / Enterprise.
    // A workspace was therefore seated on a tier no surface sells, could not see it in its own
    // catalogue, and could not pick it again. The operator DOES have a re-activate control on the
    // Plans table, so seating a retired tier is never the intended action — refuse it by name.
    let plan: Option<(Uuid, String, bool)> =
        sqlx::query_as("SELECT id, name, is_active FROM plans WHERE id = $1")
            .bind(plan_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((_, plan_name, plan_active)) = plan else {
        return Err(AppError::NotFound(format!("No plan with id {plan_id}")));
    };
    if !plan_active {
        return Err(AppError::Conflict(format!(
            "'{plan_name}' is retired, so it cannot be assigned: the customer's billing screen \
             would not list the plan they are on. Re-activate it on the Plans table first, then \
             assign it."
        )));
    }

    // ONE row per tenant is the invariant this upsert relies on (kanban t_f5494ad5): the conflict
    // target `(tenant_id)` resolves against `tenant_plans_tenant_id_key`, the unique constraint the
    // live database was missing — without it this statement aborted with Postgres 42P10 and the
    // panel's action answered 500 for every caller (migrations/000020_tenant_plans_one_row_per_tenant.sql
    // restores it). The statement itself lives in `seat_plan_for_tenant` so this panel action and the
    // paid-checkout seat (kanban t_3c97243d) cannot drift apart; the panel names the cycle, hence
    // `Some(billing_cycle)`.
    seat_plan_for_tenant(&state, tenant_id, plan_id, Some(billing_cycle)).await?;

    // Credit the referring affiliate if this is a paid-plan assignment.
    attribute_plan_upgrade(&state, tenant_id, plan_id).await;

    Ok(Json(json!({"message": "Plan assigned to account"})))
}

#[cfg(test)]
mod swallow_proof {
    //! t_08faed51: `attribute_plan_upgrade` used to run its two lookups behind
    //! `.ok().flatten()`, so a DB failure returned silently instead of logging. Its only
    //! live caller (`admin_assign_plan`) writes `tenant_plans` first, and no lock can
    //! block this lookup without blocking that write, so the failure is forced here
    //! against a Postgres that genuinely refuses the connection.
    use super::*;
    use crate::handlers::swallow_tests::{capture, dead_pool, test_state};

    #[test]
    fn attribute_plan_upgrade_logs_a_failed_plan_lookup() {
        let (_out, log) = capture(|| {
            let state: AppState = test_state();
            async move {
                attribute_plan_upgrade(&state, Uuid::nil(), Uuid::nil()).await;
            }
        });
        assert!(
            log.contains("attribute_plan_upgrade: plan lookup failed"),
            "the failed lookup must be logged: {log}"
        );
    }

    #[test]
    fn attribute_plan_upgrade_old_shape_was_silent() {
        // control leg: the exact pre-fix expression on the same dead pool.
        let (plan, log) = capture(|| {
            let pool = dead_pool();
            async move {
                let p: Option<(String, Option<f64>)> =
                    sqlx::query_as("SELECT name, price_monthly::float8 FROM plans WHERE id = $1")
                        .bind(Uuid::nil())
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten();
                p
            }
        });
        assert!(
            plan.is_none(),
            "control: the pre-fix shape returned None for a failed query"
        );
        assert!(
            !log.contains("attribute_plan_upgrade"),
            "control: the pre-fix shape logged nothing at all: {log}"
        );
    }
}
