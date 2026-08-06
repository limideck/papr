use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::Json;
use papr_core::auth;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let users = auth::list_users(&conn).map_err(ApiError::from)?;
    Ok(Json(json!(users)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserBody {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub is_admin: bool,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateUserBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = auth::create_user(&conn, &body.username, &body.password, body.is_admin)
        .map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    if id == user.id() {
        return Err(ApiError::bad_request("cannotDeleteSelf"));
    }
    let conn = state.db.lock().await;
    auth::delete_user_sessions(&conn, id).map_err(ApiError::from)?;
    auth::delete_user(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchUserBody {
    pub is_admin: Option<bool>,
    /// When set, resets the user's password (admin-only; no old-password check).
    pub password: Option<String>,
}

pub async fn patch(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<PatchUserBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    if body.is_admin.is_none() && body.password.is_none() {
        return Err(ApiError::bad_request("nothingToUpdate"));
    }
    // Prevent an admin from demoting themselves and locking everyone out.
    if body.is_admin == Some(false) && id == user.id() {
        return Err(ApiError::bad_request("cannotDemoteSelf"));
    }
    let conn = state.db.lock().await;
    if auth::get_user(&conn, id)
        .map_err(ApiError::from)?
        .is_none()
    {
        return Err(ApiError::bad_request("userNotFound"));
    }
    if let Some(is_admin) = body.is_admin {
        auth::set_user_admin(&conn, id, is_admin).map_err(ApiError::from)?;
    }
    if let Some(password) = body.password {
        auth::set_password(&conn, id, &password).map_err(ApiError::from)?;
        // Force re-login after an admin password reset.
        auth::delete_user_sessions(&conn, id).map_err(ApiError::from)?;
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordBody {
    pub old_password: String,
    pub new_password: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ChangePasswordBody>,
) -> ApiResult<Json<Value>> {
    if body.new_password.len() < 6 {
        return Err(ApiError::bad_request("passwordTooShort"));
    }
    if body.new_password == body.old_password {
        return Err(ApiError::bad_request("passwordUnchanged"));
    }
    let conn = state.db.lock().await;
    let Some((_, hash)) =
        auth::find_user_by_username(&conn, &user.0.username).map_err(ApiError::from)?
    else {
        return Err(ApiError::unauthorized());
    };
    if !auth::verify_password(&body.old_password, &hash) {
        return Err(ApiError::bad_request("incorrectPassword"));
    }
    auth::set_password(&conn, user.id(), &body.new_password).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
