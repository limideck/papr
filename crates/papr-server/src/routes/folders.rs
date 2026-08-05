use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn list(State(state): State<AppState>, _user: AuthUser) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let folders = db::list_folders(&conn).map_err(ApiError::from)?;
    Ok(Json(json!(folders)))
}

#[derive(Deserialize)]
pub struct NameBody {
    pub name: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_folder(&conn, &body.name).map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

pub async fn rename(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<NameBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::rename_folder(&conn, id, &body.name).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_folder(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
