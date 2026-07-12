use axum::{
    extract::{Extension, State},
    Json,
};

use crate::{
    config::Claims,
    error::AppError,
    models::setting::{TenantSetting, UpdateSettingsRequest},
    state::AppState,
};

pub async fn get_settings(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<TenantSetting>>, AppError> {
    let items = sqlx::query_as::<_, TenantSetting>(
        "SELECT * FROM tenant_settings WHERE tenant_id = $1",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn update_settings(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    for entry in req.settings {
        sqlx::query(
            "INSERT INTO tenant_settings (tenant_id, key, value) VALUES ($1, $2, $3) ON CONFLICT (tenant_id, key) DO UPDATE SET value = $3",
        )
        .bind(claims.aid)
        .bind(&entry.key)
        .bind(&entry.value)
        .execute(&state.pool)
        .await?;
    }

    Ok(Json(serde_json::json!({"message": "Settings updated"})))
}
