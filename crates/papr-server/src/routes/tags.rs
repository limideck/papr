use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::Json;
use papr_core::db;
use papr_core::models::TAG_KIND_INTEREST;
use papr_core::user_db;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct ListQuery {
    /// Optional `interest` | `ai` filter.
    pub kind: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let kind = q.kind.as_deref();
    Ok(Json(json!(
        user_db::list_tags_for_user(&conn, user.id(), kind).map_err(ApiError::from)?
    )))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
    /// Defaults to `interest` (admin vocabulary). AI tags are normally
    /// created by the worker; admins may still create them explicitly.
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    TAG_KIND_INTEREST.to_string()
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_tag(&conn, &body.name, &body.kind).map_err(ApiError::from)?;
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
pub struct CleanupEmptyBody {
    /// Must be `ai`. Interest tags are never bulk-deleted when unused.
    pub kind: String,
}

/// Delete unused AI tags (`article_count = 0`). Admin only.
/// Interest cleanup is rejected — empty interest tags stay as vocabulary.
pub async fn cleanup_empty(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CleanupEmptyBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let deleted = db::delete_empty_tags(&conn, &body.kind).map_err(ApiError::from)?;
    Ok(Json(json!({ "deleted": deleted })))
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
    // Attach (and create-via-toggle) stays admin-only. Any authenticated
    // reader may detach a tag from an article — especially useful for
    // clearing unwanted AI tags without leaving the reader.
    if body.on {
        user.require_admin()?;
    }
    let conn = state.db.lock().await;
    db::set_article_tag(&conn, article_id, tag_id, body.on).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ListAliasesQuery {
    pub tag_id: Option<i64>,
    /// Optional `interest` | `ai` filter.
    pub kind: Option<String>,
}

pub async fn list_aliases(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListAliasesQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    Ok(Json(json!(
        db::list_tag_aliases(&conn, q.tag_id, q.kind.as_deref()).map_err(ApiError::from)?
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAliasBody {
    pub tag_id: i64,
    pub alias: String,
}

pub async fn create_alias(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateAliasBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_tag_alias(&conn, body.tag_id, &body.alias).map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct UpdateAliasBody {
    pub alias: String,
}

pub async fn update_alias(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateAliasBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::rename_tag_alias(&conn, id, &body.alias).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_alias(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_tag_alias(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
