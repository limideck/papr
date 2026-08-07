//! Auto-tag queue status and backfill (admin).

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Query, State};
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct StatusQuery {
    /// Window for tagged/untagged hint (same basis as backfill).
    /// `0` = entire library. Default 7.
    #[serde(default = "default_days")]
    pub days: i64,
}

/// `GET /api/auto-tag/status` — queue backlog + enabled flags (admin).
pub async fn status(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let days = q.days.clamp(0, 365);
    let conn = state.db.lock().await;
    let interest_enabled = db::setting_flag(&conn, "auto_tag_enabled", false);
    let ai_enabled = db::setting_flag(&conn, "ai_tag_enabled", false);
    let queue = db::auto_tag_queue_status(&conn).map_err(ApiError::from)?;
    let window = db::auto_tag_window_stats(&conn, days).map_err(ApiError::from)?;
    Ok(Json(json!({
        "enabled": interest_enabled || ai_enabled,
        "interestEnabled": interest_enabled,
        "aiEnabled": ai_enabled,
        "pending": queue.pending,
        "processing": queue.processing,
        "failed": queue.failed,
        "done": queue.done,
        "lastError": queue.last_error,
        "windowDays": window.days,
        "articlesInWindow": window.articles,
        "untaggedInWindow": window.untagged,
        "taggedInWindow": window.tagged,
    })))
}

#[derive(Deserialize)]
pub struct BackfillBody {
    /// Re-enqueue articles from the last N days (default 7, max 365).
    /// `0` = entire library (no date filter).
    #[serde(default = "default_days")]
    pub days: i64,
    /// When true, also reset `done` jobs that already have tags.
    /// Default false: never-queued + failed + done-with-zero-tags.
    #[serde(default)]
    pub force: bool,
}

fn default_days() -> i64 {
    7
}

/// `POST /api/auto-tag/backfill` — enqueue articles for tagging (admin).
///
/// Default: never-queued, `failed`, and queue rows with **zero tags**
/// (including soft-empty `done`). Skips `done` articles that already have tags.
/// `force: true` also resets those tagged `done` rows.
/// `days: 0` scans the whole library (no publish/fetch date window).
pub async fn backfill(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<BackfillBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let days = body.days.clamp(0, 365);
    let conn = state.db.lock().await;
    let enqueued =
        db::enqueue_auto_tag_backfill(&conn, days, body.force).map_err(ApiError::from)?;
    Ok(Json(json!({
        "ok": true,
        "days": days,
        "force": body.force,
        "enqueued": enqueued,
    })))
}

/// `POST /api/auto-tag/clear-queue` — soft pause: drop pending/processing/failed
/// (keep `done`). Does not auto-backfill; admin re-enqueues via backfill.
pub async fn clear_queue(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let cleared = db::clear_auto_tag_queue(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({
        "cleared": cleared,
    })))
}
