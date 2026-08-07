//! Stub handler for export-templates

use axum::{extract::{Path, Query, State}, Json};
use crate::error::AppError;
use crate::state::AppState;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct ListQuery {
    #[allow(dead_code)]
    pub limit: Option<i64>,
    #[allow(dead_code)]
    pub offset: Option<i64>,
    #[allow(dead_code)]
    pub search: Option<String>,
}

pub async fn list(
    State(_state): State<AppState>,
    Query(_query): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(json!([])))
}

pub async fn create(
    State(_state): State<AppState>,
    Json(_body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(json!({"created": true})))
}

pub async fn get(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(json!({"id": id})))
}

pub async fn update(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(_body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(json!({"updated": true, "id": id})))
}

pub async fn delete(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(json!({"deleted": true, "id": id})))
}
