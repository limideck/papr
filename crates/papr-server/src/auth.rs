//! Session cookie auth extractor and login helpers.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue};
use papr_core::auth;

pub const SESSION_COOKIE: &str = "session";

pub fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("session=") {
            let token = rest.split(';').next().unwrap_or(rest).trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

pub fn set_session_cookie(headers: &mut HeaderMap, token: &str) {
    let value = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000"
    );
    if let Ok(v) = HeaderValue::from_str(&value) {
        headers.insert(header::SET_COOKIE, v);
    }
}

pub fn clear_session_cookie(headers: &mut HeaderMap) {
    let value = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if let Ok(v) = HeaderValue::from_str(&value) {
        headers.insert(header::SET_COOKIE, v);
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = session_token_from_headers(&parts.headers)
            .ok_or_else(ApiError::unauthorized)?;
        let conn = state.db.lock().await;
        let user = auth::user_for_session(&conn, &token)
            .map_err(ApiError::from)?
            .ok_or_else(ApiError::unauthorized)?;
        Ok(AuthUser(user))
    }
}

pub async fn login(
    state: &AppState,
    username: &str,
    password: &str,
) -> ApiResult<(auth::User, String)> {
    let conn = state.db.lock().await;
    let Some((user, hash)) = auth::find_user_by_username(&conn, username).map_err(ApiError::from)?
    else {
        return Err(ApiError::unauthorized());
    };
    if !auth::verify_password(password, &hash) {
        return Err(ApiError::unauthorized());
    }
    let token = auth::create_session(&conn, user.id).map_err(ApiError::from)?;
    Ok((user, token))
}
