//! Highlights API (feature F7): create, list, update (note/colour), delete.
//! Straight passthrough to `papr_core::db` — highlights carry no server-side
//! authz beyond the session.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::Json;
use papr_core::db;
use papr_core::error::AppError;
use papr_core::models::Highlight;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewHighlightBody {
    pub article_id: i64,
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
    pub text_offset: i64,
    pub color: String,
    pub note: String,
}

/// `POST /api/highlights` — create a highlight, returning its new id.
pub async fn create(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<NewHighlightBody>,
) -> ApiResult<Json<i64>> {
    if body.quote.trim().is_empty() {
        return Err(ApiError::from(AppError::code("emptyHighlight")));
    }
    let conn = state.db.lock().await;
    let id = db::insert_highlight(
        &conn,
        &db::NewHighlight {
            article_id: body.article_id,
            quote: &body.quote,
            prefix: &body.prefix,
            suffix: &body.suffix,
            text_offset: body.text_offset,
            color: &body.color,
            note: &body.note,
        },
    )
    .map_err(ApiError::from)?;
    Ok(Json(id))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub article_id: Option<i64>,
}

/// `GET /api/highlights` — every highlight, or just one article's when
/// `?articleId=` is given.
pub async fn list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<Highlight>>> {
    let conn = state.db.lock().await;
    let rows = match q.article_id {
        Some(article_id) => db::list_highlights(&conn, article_id),
        None => db::list_all_highlights(&conn),
    }
    .map_err(ApiError::from)?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchBody {
    pub note: Option<String>,
    pub color: Option<String>,
}

/// `PATCH /api/highlights/{id}` — replace the note and/or colour.
pub async fn patch(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    if let Some(note) = &body.note {
        db::update_highlight_note(&conn, id, note).map_err(ApiError::from)?;
    }
    if let Some(color) = &body.color {
        db::set_highlight_color(&conn, id, color).map_err(ApiError::from)?;
    }
    Ok(Json(json!({ "ok": true })))
}

/// `DELETE /api/highlights/{id}`
pub async fn delete(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    db::delete_highlight(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
