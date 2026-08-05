use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::State;
use axum::Json;
use papr_core::db;
use papr_core::ingestion::parse;
use papr_core::opml;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct ImportBody {
    pub content: String,
}

pub async fn import(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ImportBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let imported = opml::parse(&body.content).map_err(ApiError::from)?;
    let conn = state.db.lock().await;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| ApiError::from(papr_core::error::AppError::from(e)))?;
    let mut added = 0usize;
    for feed in imported {
        if db::find_feed_by_url(&tx, &feed.feed_url)
            .map_err(ApiError::from)?
            .is_some()
        {
            continue;
        }
        let folder_id = match &feed.folder {
            Some(name) => Some(db::folder_id_by_name(&tx, name).map_err(ApiError::from)?),
            None => None,
        };
        let source_type = parse::detect_source_type(&feed.feed_url);
        db::insert_feed(
            &tx,
            &feed.feed_url,
            None,
            &feed.title,
            None,
            source_type,
            folder_id,
        )
        .map_err(ApiError::from)?;
        added += 1;
    }
    tx.commit()
        .map_err(|e| ApiError::from(papr_core::error::AppError::from(e)))?;
    Ok(Json(json!({ "count": added })))
}

pub async fn export(State(state): State<AppState>, _user: AuthUser) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let feeds = db::feeds_for_export(&conn).map_err(ApiError::from)?;
    let xml = opml::build(&feeds).map_err(ApiError::from)?;
    Ok(Json(json!({ "content": xml })))
}
