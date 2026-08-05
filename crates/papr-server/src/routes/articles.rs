use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::Json;
use papr_core::db;
use papr_core::extraction;
use papr_core::ingestion::fetch;
use papr_core::models::ArticleQuery;
use papr_core::sanitize;
use papr_core::user_db;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    pub kind: Option<String>,
    pub value: Option<i64>,
    #[serde(default)]
    pub unread_only: bool,
    pub search: Option<String>,
    #[serde(default)]
    pub oldest_first: bool,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

fn parse_article_query(kind: Option<&str>, value: Option<i64>) -> ArticleQuery {
    match kind.unwrap_or("all") {
        "unread" => ArticleQuery::Unread,
        "starred" => ArticleQuery::Starred,
        "readLater" | "read_later" => ArticleQuery::ReadLater,
        "feed" => ArticleQuery::Feed(value.unwrap_or(0)),
        "folder" => ArticleQuery::Folder(value.unwrap_or(0)),
        "tag" => ArticleQuery::Tag(value.unwrap_or(0)),
        _ => ArticleQuery::All,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListBody {
    pub query: ArticleQueryBody,
    #[serde(default)]
    pub unread_only: bool,
    pub search: Option<String>,
    #[serde(default)]
    pub oldest_first: bool,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ArticleQueryBody {
    All,
    Unread,
    Starred,
    ReadLater,
    Feed(i64),
    Folder(i64),
    Tag(i64),
}

impl From<ArticleQueryBody> for ArticleQuery {
    fn from(q: ArticleQueryBody) -> Self {
        match q {
            ArticleQueryBody::All => ArticleQuery::All,
            ArticleQueryBody::Unread => ArticleQuery::Unread,
            ArticleQueryBody::Starred => ArticleQuery::Starred,
            ArticleQueryBody::ReadLater => ArticleQuery::ReadLater,
            ArticleQueryBody::Feed(v) => ArticleQuery::Feed(v),
            ArticleQueryBody::Folder(v) => ArticleQuery::Folder(v),
            ArticleQueryBody::Tag(v) => ArticleQuery::Tag(v),
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let query = parse_article_query(q.kind.as_deref(), q.value);
    let conn = state.db.lock().await;
    let rows = user_db::list_articles_for_user(
        &conn,
        user.id(),
        &query,
        q.unread_only,
        q.search.as_deref(),
        q.oldest_first,
        q.limit,
        q.offset,
    )
    .map_err(ApiError::from)?;
    Ok(Json(json!(rows)))
}

pub async fn list_post(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ListBody>,
) -> ApiResult<Json<Value>> {
    let query = ArticleQuery::from(body.query);
    let conn = state.db.lock().await;
    let rows = user_db::list_articles_for_user(
        &conn,
        user.id(),
        &query,
        body.unread_only,
        body.search.as_deref(),
        body.oldest_first,
        body.limit,
        body.offset,
    )
    .map_err(ApiError::from)?;
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexBody {
    pub query: ArticleQueryBody,
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default)]
    pub oldest_first: bool,
    pub article_id: i64,
}

pub async fn index(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<IndexBody>,
) -> ApiResult<Json<Value>> {
    let query = ArticleQuery::from(body.query);
    let conn = state.db.lock().await;
    let pos = user_db::article_index_for_user(
        &conn,
        user.id(),
        &query,
        body.unread_only,
        body.oldest_first,
        body.article_id,
    )
    .map_err(ApiError::from)?;
    Ok(Json(json!(pos)))
}

pub async fn get(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let detail = user_db::get_article_for_user(&conn, user.id(), id).map_err(ApiError::from)?;
    Ok(Json(json!(detail)))
}

#[derive(Deserialize)]
pub struct FlagBody {
    /// Prefer `value`; `read` / `starred` accepted for older clients.
    #[serde(alias = "read", alias = "starred")]
    pub value: bool,
}

pub async fn mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<FlagBody>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    user_db::set_read_for_user(&conn, user.id(), id, body.value).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn mark_starred(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<FlagBody>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    user_db::set_starred_for_user(&conn, user.id(), id, body.value).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn mark_read_later(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<FlagBody>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    user_db::set_read_later_for_user(&conn, user.id(), id, body.value).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkAllBody {
    pub query: ArticleQueryBody,
}

pub async fn mark_all_read(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<MarkAllBody>,
) -> ApiResult<Json<Value>> {
    let query = ArticleQuery::from(body.query);
    let conn = state.db.lock().await;
    let n = user_db::mark_all_read_for_user(&conn, user.id(), &query).map_err(ApiError::from)?;
    Ok(Json(json!({ "count": n })))
}

pub async fn smart_counts(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let (unread, starred, read_later) =
        user_db::smart_counts_for_user(&conn, user.id()).map_err(ApiError::from)?;
    Ok(Json(json!({
        "unread": unread,
        "starred": starred,
        "readLater": read_later,
    })))
}

pub async fn extract_fulltext(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let _ = user;
    let url = {
        let conn = state.db.lock().await;
        db::get_article(&conn, id)
            .map_err(ApiError::from)?
            .url
            .ok_or_else(|| ApiError::from(papr_core::error::AppError::code("noArticleUrl")))?
    };
    let (bytes, ct, final_url) = fetch::get(&state.http, &url)
        .await
        .map_err(ApiError::from)?;
    let html = fetch::decode_html(&bytes, ct.as_deref());
    let lead_image = extraction::lead_image(&html, &final_url);
    let extraction_url = final_url.clone();
    let extracted = tokio::task::spawn_blocking(move || {
        extraction::extract_article(&html, &extraction_url)
    })
    .await
    .map_err(|e| ApiError::other(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(ApiError::from)?;
    let image_url = sanitize::first_image(&extracted).or(lead_image);
    let conn = state.db.lock().await;
    db::set_extracted_html(&conn, id, &extracted, image_url.as_deref()).map_err(ApiError::from)?;
    Ok(Json(json!({ "html": extracted })))
}
