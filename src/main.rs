#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

mod email;
mod auth;
mod config;
mod db;
mod error;
mod handlers;
mod models;
mod routes;
mod features;
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

    let workflowswift_url = std::env::var("WORKFLOWSWIFT_URL")
        .unwrap_or_else(|_| "http://localhost:8085/api/incoming".into());

    let app_state = state::AppState {
        pool,
        config: cfg.clone(),
        workflowswift_url,
    };

    let app = routes::create_router(app_state);
    let addr: SocketAddr = format!("{}:{}", cfg.server_host, cfg.server_port).parse()?;
    tracing::info!("MissedCall Respondr starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
