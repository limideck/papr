use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use papr_core::db;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

const IMAGE_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "service": "papr-server" }))
}

pub async fn storage_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let (db_bytes, article_count, feed_count) =
        db::storage_stats(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({
        "dbBytes": db_bytes,
        "articleCount": article_count,
        "feedCount": feed_count,
    })))
}

#[derive(Deserialize)]
pub struct CleanupBody {
    pub days: i64,
}

pub async fn cleanup(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CleanupBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let n = db::cleanup_old_articles(&conn, body.days).map_err(ApiError::from)?;
    Ok(Json(json!({ "count": n })))
}

pub async fn vacuum(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    conn.execute_batch("VACUUM").map_err(|e| ApiError::from(papr_core::error::AppError::from(e)))?;
    Ok(Json(json!({ "ok": true })))
}

/// Wipe all feeds (articles cascade), folders. Settings are kept.
pub async fn clear_all_data(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::clear_all_data(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

/// Clear every stored setting row. UI prefs in localStorage are cleared by the client.
pub async fn reset_settings(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::reset_settings(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchImageQuery {
    pub url: String,
    pub page_url: Option<String>,
}

fn referer_candidates(image_url: &str, page_url: Option<&str>) -> Vec<Option<String>> {
    let mut out = vec![None];
    if let Ok(u) = Url::parse(image_url) {
        let origin = u.origin().ascii_serialization();
        if origin != "null" {
            out.push(Some(format!("{origin}/")));
        }
    }
    if let Some(p) = page_url {
        if (p.starts_with("http://") || p.starts_with("https://")) && Url::parse(p).is_ok() {
            let candidate = Some(p.to_string());
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

pub async fn fetch_image(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<FetchImageQuery>,
) -> ApiResult<(StatusCode, HeaderMap, Bytes)> {
    if !(q.url.starts_with("http://") || q.url.starts_with("https://")) {
        return Err(ApiError::from(papr_core::error::AppError::code("badImageUrl")));
    }
    let mut last_err = ApiError::from(papr_core::error::AppError::code("badImageUrl"));
    for referer in referer_candidates(&q.url, q.page_url.as_deref()) {
        let mut req = state.http.get(&q.url).header("User-Agent", IMAGE_UA);
        if let Some(r) = &referer {
            req = req.header("Referer", r.as_str());
        }
        match req.send().await {
            Err(e) => last_err = ApiError::from(papr_core::error::AppError::from(e)),
            Ok(resp) => match resp.error_for_status() {
                Err(e) => last_err = ApiError::from(papr_core::error::AppError::from(e)),
                Ok(resp) => {
                    if resp.content_length().is_some_and(|n| n > MAX_IMAGE_BYTES) {
                        return Err(ApiError::from(papr_core::error::AppError::code(
                            "imageTooLarge",
                        )));
                    }
                    let ct = resp
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("application/octet-stream")
                        .to_string();
                    let bytes = resp.bytes().await.map_err(|e| {
                        ApiError::from(papr_core::error::AppError::from(e))
                    })?;
                    if bytes.len() as u64 > MAX_IMAGE_BYTES {
                        return Err(ApiError::from(papr_core::error::AppError::code(
                            "imageTooLarge",
                        )));
                    }
                    let mut headers = HeaderMap::new();
                    if let Ok(v) = header::HeaderValue::from_str(&ct) {
                        headers.insert(header::CONTENT_TYPE, v);
                    }
                    return Ok((StatusCode::OK, headers, bytes));
                }
            },
        }
    }
    Err(last_err)
}
