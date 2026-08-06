//! Auto-tag queue status and backfill (admin).

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::State;
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};

/// `GET /api/auto-tag/status` — queue backlog + enabled flags (admin).
pub async fn status(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let interest_enabled = db::setting_flag(&conn, "auto_tag_enabled", false);
    let ai_enabled = db::setting_flag(&conn, "ai_tag_enabled", false);
    let queue = db::auto_tag_queue_status(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({
        "enabled": interest_enabled || ai_enabled,
        "interestEnabled": interest_enabled,
        "aiEnabled": ai_enabled,
        "pending": queue.pending,
        "failed": queue.failed,
        "lastError": queue.last_error,
    })))
}

#[derive(Deserialize)]
pub struct BackfillBody {
    /// Re-enqueue articles from the last N days (default 7, min 1, max 365).
    #[serde(default = "default_days")]
    pub days: i64,
}

fn default_days() -> i64 {
    7
}

/// `POST /api/auto-tag/backfill` — enqueue recent articles for tagging (admin).
pub async fn backfill(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<BackfillBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let days = body.days.clamp(1, 365);
    let conn = state.db.lock().await;
    let enqueued = db::enqueue_auto_tag_backfill(&conn, days).map_err(ApiError::from)?;
    Ok(Json(json!({
        "ok": true,
        "days": days,
        "enqueued": enqueued,
    })))
}
