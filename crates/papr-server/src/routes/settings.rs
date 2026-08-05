use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};

/// Setting keys that must not leak to non-admin clients.
fn is_secret_setting(key: &str) -> bool {
    matches!(key, "ai_api_key" | "freshrss_pass" | "freshrss_auth")
}

/// Per-client UI prefs any authenticated user may write. Shared infrastructure
/// (AI keys, network, retention, feed ingestion) stays admin-only. Note: these
/// keys are still stored globally for MVP, so the last writer wins.
fn is_user_pref_setting(key: &str) -> bool {
    matches!(
        key,
        "list_translate_mode" | "translate_engine" | "translate_target_lang"
    )
}

pub async fn get(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let raw = db::get_setting(&conn, &key).map_err(ApiError::from)?;
    // Non-admins never see secret values — empty string when the key exists,
    // null when missing — so the UI can show a blank password field.
    let value = if is_secret_setting(&key) && !user.0.is_admin {
        raw.map(|_| String::new())
    } else {
        raw
    };
    Ok(Json(json!({ "key": key, "value": value })))
}

#[derive(Deserialize)]
pub struct SetBody {
    pub value: String,
}

pub async fn set(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    Json(body): Json<SetBody>,
) -> ApiResult<Json<Value>> {
    // Translate UI prefs are writable by any signed-in user (article-list
    // auto-translate toggle, reader defaults). Shared ingestion / AI / network
    // settings still require admin.
    if !is_user_pref_setting(&key) {
        user.require_admin()?;
    }
    let conn = state.db.lock().await;
    db::set_setting(&conn, &key, &body.value).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
