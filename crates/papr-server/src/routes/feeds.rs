use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::Json;
use papr_core::db;
use papr_core::ingestion::discovery;
use papr_core::ingestion::{fetch, parse, refresh, sources};
use papr_core::ingestion::sources::Normalized;
use papr_core::models::{Feed, SourceType};
use papr_core::user_db;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let feeds = user_db::list_feeds_for_user(&conn, user.id()).map_err(ApiError::from)?;
    Ok(Json(json!(feeds)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddFeedBody {
    pub url: String,
    pub folder_id: Option<i64>,
}

pub async fn add(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<AddFeedBody>,
) -> ApiResult<Json<Feed>> {
    user.require_admin()?;
    let feed = add_feed_inner(&state, &body.url, body.folder_id).await?;
    Ok(Json(feed))
}

async fn add_feed_inner(
    state: &AppState,
    url: &str,
    folder_id: Option<i64>,
) -> ApiResult<Feed> {
    let client = &state.http;
    let url = if url
        .trim()
        .get(..9)
        .is_some_and(|s| s.eq_ignore_ascii_case("rsshub://"))
    {
        let instance = {
            let conn = state.db.lock().await;
            db::get_setting(&conn, "rsshub_instance")
                .map_err(ApiError::from)?
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| sources::DEFAULT_RSSHUB_INSTANCE.to_string())
        };
        sources::expand_rsshub(url, &instance).unwrap_or_else(|| url.to_string())
    } else {
        url.to_string()
    };

    let (effective_url, forced_type): (String, Option<SourceType>) =
        match sources::normalize_source(&url) {
            Normalized::Feed { url, source_type } => (url, Some(source_type)),
            Normalized::NeedsYoutubeResolution { page_url } => {
                let (page_bytes, ct, _) = fetch::get(client, &page_url)
                    .await
                    .map_err(ApiError::from)?;
                let html = fetch::decode_html(&page_bytes, ct.as_deref());
                let channel_id = sources::extract_channel_id(&html)
                    .ok_or_else(|| ApiError::from(papr_core::error::AppError::code("youtubeChannelNotFound")))?;
                (
                    sources::youtube_feed_url(&channel_id),
                    Some(SourceType::Youtube),
                )
            }
            Normalized::Untouched => (url.clone(), None),
        };

    let (bytes, ct, final_url) = fetch::get(client, &effective_url)
        .await
        .map_err(ApiError::from)?;

    let (feed_url, feed_bytes) = if parse::looks_like_feed(&bytes) {
        (final_url, bytes)
    } else {
        let html = fetch::decode_html(&bytes, ct.as_deref());
        let candidates = parse::discover_feeds(&html, &final_url);
        let candidate = candidates
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::from(papr_core::error::AppError::code("noFeedFound")))?;
        let (fb, _, _) = fetch::get(client, &candidate).await.map_err(ApiError::from)?;
        (candidate, fb)
    };

    let parsed = parse::parse_feed(&feed_bytes, &feed_url).map_err(ApiError::from)?;
    let source_type = match forced_type {
        Some(t) => t,
        None => parse::refine_source_type(
            parse::detect_source_type(&feed_url),
            &parsed,
            &feed_url,
        ),
    };

    let title = parsed
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| feed_url.clone());
    let favicon = parsed.icon.clone().or_else(|| {
        parsed
            .site_url
            .as_deref()
            .and_then(|s| Url::parse(s).ok())
            .and_then(|u| u.host_str().map(String::from))
            .map(|h| format!("https://www.google.com/s2/favicons?domain={h}&sz=64"))
    });

    let conn = state.db.lock().await;
    if db::find_feed_by_url(&conn, &feed_url)
        .map_err(ApiError::from)?
        .is_some()
    {
        return Err(ApiError::from(papr_core::error::AppError::code(
            "alreadySubscribed",
        )));
    }
    let feed_id = db::insert_feed(
        &conn,
        &feed_url,
        parsed.site_url.as_deref(),
        &title,
        parsed.description.as_deref(),
        source_type,
        folder_id,
    )
    .map_err(ApiError::from)?;
    if let Some(fav) = &favicon {
        db::update_feed_meta(&conn, feed_id, None, None, None, Some(fav)).map_err(ApiError::from)?;
    }
    let dedup = db::setting_flag(&conn, "dedup_enabled", true);
    let rules = db::active_rules(&conn).unwrap_or_default();
    for article in &parsed.articles {
        let _ = db::upsert_article(&conn, feed_id, article, dedup, &rules);
    }
    let _ = db::touch_feed(&conn, feed_id);
    let last_fetched_at = db::feed_last_fetched(&conn, feed_id).ok().flatten();
    let unread = db::count_feed_unread(&conn, feed_id).map_err(ApiError::from)?;

    Ok(Feed {
        id: feed_id,
        feed_url,
        site_url: parsed.site_url,
        title,
        description: parsed.description,
        favicon_url: favicon,
        folder_id,
        source_type: source_type.as_str().to_string(),
        last_fetched_at,
        fetch_error: None,
        unread_count: unread,
        refresh_interval_min: None,
        auto_translate: false,
        open_mode: None,
    })
}

#[derive(Deserialize)]
pub struct DiscoverQuery {
    pub query: String,
    #[serde(default = "default_lang")]
    pub lang: String,
}

fn default_lang() -> String {
    "en".into()
}

pub async fn discover(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<DiscoverQuery>,
) -> ApiResult<Json<Value>> {
    let mut results = Vec::new();
    if discovery::looks_like_url(&q.query) {
        let target = discovery::normalize_query_url(&q.query);
        if let Ok((bytes, ct, final_url)) = fetch::get(&state.http, &target).await {
            let html = fetch::decode_html(&bytes, ct.as_deref());
            for feed_url in parse::discover_feeds(&html, &final_url) {
                results.push(json!({
                    "title": feed_url,
                    "feedUrl": feed_url,
                    "siteUrl": final_url,
                    "category": null,
                    "description": null,
                    "fromDirectory": false,
                }));
            }
        }
    }
    let dir = discovery::search_directory(&q.query, &q.lang);
    for d in dir {
        results.push(json!({
            "title": d.title,
            "feedUrl": d.feed_url,
            "siteUrl": d.site_url,
            "category": d.category,
            "description": d.description,
            "fromDirectory": true,
        }));
    }
    Ok(Json(json!(results)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFeedBody {
    pub title: Option<String>,
    pub folder_id: Option<Option<i64>>,
    pub refresh_interval_min: Option<Option<i64>>,
    pub auto_translate: Option<bool>,
    pub open_mode: Option<Option<String>>,
}

pub async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateFeedBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    if let Some(title) = body.title {
        db::rename_feed(&conn, id, &title).map_err(ApiError::from)?;
    }
    if let Some(folder_id) = body.folder_id {
        db::move_feed(&conn, id, folder_id).map_err(ApiError::from)?;
    }
    if let Some(mins) = body.refresh_interval_min {
        db::set_feed_refresh_interval(&conn, id, mins).map_err(ApiError::from)?;
    }
    if let Some(enabled) = body.auto_translate {
        db::set_feed_auto_translate(&conn, id, enabled).map_err(ApiError::from)?;
    }
    if let Some(mode) = body.open_mode {
        db::set_feed_open_mode(&conn, id, mode.as_deref()).map_err(ApiError::from)?;
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_feed(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshBody {
    pub feed_id: Option<i64>,
    pub folder_id: Option<i64>,
}

pub async fn refresh(
    State(state): State<AppState>,
    user: AuthUser,
    body: Option<Json<RefreshBody>>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let body = body.map(|j| j.0).unwrap_or(RefreshBody {
        feed_id: None,
        folder_id: None,
    });
    let scope = if let Some(id) = body.feed_id {
        refresh::RefreshScope::Feed(id)
    } else if let Some(id) = body.folder_id {
        refresh::RefreshScope::Folder(id)
    } else {
        refresh::RefreshScope::All
    };
    let db = state.db.clone();
    let client = state.http.clone();
    let summary = refresh::refresh_core(&db, &client, scope, |_| {})
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({
        "newArticles": summary.new_articles,
        "ran": summary.ran,
    })))
}
