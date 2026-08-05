use crate::auth::{self, clear_session_cookie, set_session_cookie, session_token_from_headers};
use crate::error::ApiResult;
use crate::state::{AppState, AuthUser};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> ApiResult<(StatusCode, HeaderMap, Json<Value>)> {
    let (user, token) = auth::login(&state, &body.username, &body.password).await?;
    let mut headers = HeaderMap::new();
    set_session_cookie(&mut headers, &token);
    Ok((
        StatusCode::OK,
        headers,
        Json(json!({
            "id": user.id,
            "username": user.username,
            "isAdmin": user.is_admin,
        })),
    ))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, HeaderMap, Json<Value>)> {
    if let Some(token) = session_token_from_headers(&headers) {
        let conn = state.db.lock().await;
        let _ = papr_core::auth::delete_session(&conn, &token);
    }
    let mut out = HeaderMap::new();
    clear_session_cookie(&mut out);
    Ok((StatusCode::OK, out, Json(json!({ "ok": true }))))
}

pub async fn me(user: AuthUser) -> Json<Value> {
    Json(json!({
        "id": user.0.id,
        "username": user.0.username,
        "isAdmin": user.0.is_admin,
        "createdAt": user.0.created_at,
    }))
}
