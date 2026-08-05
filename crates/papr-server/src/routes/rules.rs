use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn list(State(state): State<AppState>, _user: AuthUser) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    Ok(Json(json!(db::list_rules(&conn).map_err(ApiError::from)?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub name: String,
    pub feed_id: Option<i64>,
    pub field: String,
    pub query: String,
    pub action: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_rule(
        &conn,
        &body.name,
        body.feed_id,
        &body.field,
        &body.query,
        &body.action,
    )
    .map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    pub name: String,
    pub enabled: bool,
    pub feed_id: Option<i64>,
    pub field: String,
    pub query: String,
    pub action: String,
}

pub async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::update_rule(
        &conn,
        id,
        &body.name,
        body.enabled,
        body.feed_id,
        &body.field,
        &body.query,
        &body.action,
    )
    .map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_rule(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewBody {
    pub feed_id: Option<i64>,
    pub field: String,
    pub query: String,
}

pub async fn preview(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<PreviewBody>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let (count, samples) =
        db::preview_rule(&conn, body.feed_id, &body.field, &body.query).map_err(ApiError::from)?;
    Ok(Json(json!({ "count": count, "samples": samples })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyBody {
    pub feed_id: Option<i64>,
    pub field: String,
    pub query: String,
    pub action: String,
}

pub async fn apply(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ApplyBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let n = db::apply_rule_to_existing(
        &conn,
        body.feed_id,
        &body.field,
        &body.query,
        &body.action,
    )
    .map_err(ApiError::from)?;
    Ok(Json(json!({ "count": n })))
}
