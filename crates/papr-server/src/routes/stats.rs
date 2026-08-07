//! Admin overview statistics.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Query, State};
use axum::Json;
use papr_core::db;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct OverviewQuery {
    /// Calendar days of daily ingest series (default 30, max 366).
    #[serde(default = "default_days")]
    pub days: i64,
}

fn default_days() -> i64 {
    30
}

/// `GET /api/stats/overview` — admin dashboard totals + tag queue + daily ingest.
pub async fn overview(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<OverviewQuery>,
) -> ApiResult<Json<db::StatsOverview>> {
    user.require_admin()?;
    let days = q.days.clamp(1, 366);
    let conn = state.db.lock().await;
    let overview = db::stats_overview(&conn, days).map_err(ApiError::from)?;
    Ok(Json(overview))
}
