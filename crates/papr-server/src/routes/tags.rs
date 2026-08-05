use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn list(State(state): State<AppState>, _user: AuthUser) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    Ok(Json(json!(db::list_tags(&conn).map_err(ApiError::from)?)))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_tag(&conn, &body.name).map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    pub name: Option<String>,
    pub color: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    if let Some(name) = body.name {
        db::rename_tag(&conn, id, &name).map_err(ApiError::from)?;
    }
    if let Some(color) = body.color {
        db::set_tag_color(&conn, id, &color).map_err(ApiError::from)?;
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_tag(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ReorderBody {
    pub ids: Vec<i64>,
}

pub async fn reorder(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ReorderBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::reorder_tags(&conn, &body.ids).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetTagBody {
    pub on: bool,
}

pub async fn set_article_tag(
    State(state): State<AppState>,
    user: AuthUser,
    Path((article_id, tag_id)): Path<(i64, i64)>,
    Json(body): Json<SetTagBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::set_article_tag(&conn, article_id, tag_id, body.on).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
