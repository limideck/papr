use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::Json;
use papr_core::ingestion::feed_source;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let rows = feed_source::list_feed_sources(&conn).map_err(ApiError::from)?;
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBody {
    pub base_url: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = feed_source::insert_feed_source(&conn, &body.base_url).map_err(ApiError::from)?;
    let src = feed_source::get_feed_source(&conn, id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::bad_request("notFound"))?;
    Ok(Json(json!(src)))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    feed_source::delete_feed_source(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn scan_one(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let (base, source_id) = {
        let conn = state.db.lock().await;
        let src = feed_source::get_feed_source(&conn, id)
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::bad_request("notFound"))?;
        (src.base_url, src.id)
    };
    let result = feed_source::scan(&state.db, &state.http, source_id, &base).await;
    Ok(Json(json!(result)))
}

pub async fn scan_all(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let results = feed_source::sync_all(&state.db, &state.http).await;
    Ok(Json(json!(results)))
}
