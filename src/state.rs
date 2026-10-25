use crate::config::AppConfig;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub workflowswift_url: String,
    pub coreswift_url: String,
    pub funnelswift_url: String,
    /// Failed-sign-in counter, keyed by client address (`crate::login_throttle`).
    pub login_throttle: crate::login_throttle::LoginThrottle,
}
