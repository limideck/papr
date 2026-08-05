use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Query, State};
use axum::Json;
use papr_core::wordcloud;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct WordcloudQuery {
    pub days: Option<i32>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub top: Option<usize>,
}

pub async fn get(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<WordcloudQuery>,
) -> ApiResult<Json<Value>> {
    let days = q.days.unwrap_or(1);
    let from = q.from.as_deref().unwrap_or("");
    let to = q.to.as_deref().unwrap_or("");
    let range = wordcloud::resolve_range_local(days, from, to).map_err(|e| {
        ApiError::other(axum::http::StatusCode::BAD_REQUEST, e.to_string())
    })?;
    let top = q.top.unwrap_or(wordcloud::DEFAULT_TOP_N);
    let conn = state.db.lock().await;
    let cloud = wordcloud::build_for_range(&conn, &range, top).map_err(ApiError::from)?;
    Ok(Json(json!({
        "from": range.from,
        "to": range.to,
        "terms": cloud.terms,
        "scanned": cloud.scanned,
    })))
}
