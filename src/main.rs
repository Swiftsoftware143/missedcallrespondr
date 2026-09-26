mod auth;
mod config;
mod db;
mod email;
mod error;
mod features;
mod handlers;
mod models;
mod routes;
mod security;
mod state;

use std::net::SocketAddr;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let cfg = config::AppConfig::from_env();
    let pool = sqlx::PgPool::connect(&cfg.database_url).await?;

    tracing::info!("Running migrations...");
    db::run_migrations(&pool).await?;
    tracing::info!("Migrations complete");

    // BYOK at-rest encryption posture (PROVIDER_KEY_ENC_SECRET). DISABLED means provider key
    // writes fail closed rather than storing a plaintext credential.
    tracing::info!(
        "Provider key encryption: {} (AES-256 at rest, enc:v1 format)",
        if security::provider_key_crypto::is_configured() {
            "enabled"
        } else {
            "DISABLED - provider key writes will fail closed"
        }
    );

    let workflowswift_url = std::env::var("WORKFLOWSWIFT_URL")
        .unwrap_or_else(|_| "http://localhost:8085/api/incoming".into());

    // Stripe signature freshness (kanban t_4754e612): how far a delivery's `t=` stamp may be from
    // this host's clock before the receiver refuses it even though its HMAC verified. In the boot
    // log for the same reason as the provider-key posture above — an operator diagnosing "every
    // Stripe delivery is being refused" has to be able to read the value in force, and it is also
    // the value that says whether THIS box's clock is the suspect.
    tracing::info!(
        "Stripe webhook signature tolerance: {}s from this host's clock; a correctly-signed \
         delivery further away is answered 503 stripe_signature_timestamp_out_of_tolerance and \
         logged (STRIPE_WEBHOOK_TOLERANCE_SECS, clamped 30..86400, default {})",
        cfg.stripe_signature_tolerance_secs,
        handlers::checkout_handler::DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS
    );

    let coreswift_url =
        std::env::var("CORESWIFT_URL").unwrap_or_else(|_| "http://localhost:8084".into());

    let funnelswift_url =
        std::env::var("FUNNELSWIFT_URL").unwrap_or_else(|_| "http://localhost:8080".into());

    let app_state = state::AppState {
        pool,
        config: cfg.clone(),
        workflowswift_url,
        coreswift_url,
        funnelswift_url,
    };

    let app = routes::create_router(app_state);
    let addr: SocketAddr = format!("{}:{}", cfg.server_host, cfg.server_port).parse()?;
    tracing::info!("MissedCall Respondr starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
