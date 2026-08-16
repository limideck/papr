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
    /// When true, reload stopwords/entities from the shared dashboard dir.
    pub refresh: Option<bool>,
}

pub async fn get(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<WordcloudQuery>,
) -> ApiResult<Json<Value>> {
    if q.refresh.unwrap_or(false) {
        state.wordcloud.reload();
        let conn = state.db.lock().await;
        // Bump terms dict version only when on-disk file versions changed.
        let _ = state
            .wordcloud
            .with_dict(|dict| wordcloud::sync_dict_file_version(&conn, dict));
    }
    let days = q.days.unwrap_or(1);
    let from = q.from.as_deref().unwrap_or("");
    let to = q.to.as_deref().unwrap_or("");
    let custom_range = !from.is_empty() && !to.is_empty();
    let range = wordcloud::resolve_range_local(days, from, to).map_err(|e| {
        ApiError::other(axum::http::StatusCode::BAD_REQUEST, e.to_string())
    })?;
    let top = q.top.unwrap_or(wordcloud::DEFAULT_TOP_N);
    let conn = state.db.lock().await;
    let cloud = state
        .wordcloud
        .with_dict(|dict| {
            wordcloud::build_for_range_cached(
                &conn,
                &range,
                days,
                custom_range,
                top,
                Some(dict),
            )
        })
        .map_err(ApiError::from)?;
    Ok(Json(json!({
        "from": range.from,
        "to": range.to,
        "terms": cloud.terms,
        "scanned": cloud.scanned,
    })))
}

/// Read-only stopwords document from the shared dashboard config.
pub async fn get_stopwords(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    state.wordcloud.reload();
    {
        let conn = state.db.lock().await;
        let _ = state
            .wordcloud
            .with_dict(|dict| wordcloud::sync_dict_file_version(&conn, dict));
    }
    let doc = state.wordcloud.snapshot_stopwords();
    Ok(Json(json!({
        "version": doc.version,
        "words": doc.words,
    })))
}

/// Entity gazetteer (admin). Source may be shared seed, local COW, or explicit dir.
pub async fn get_entities(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    state.wordcloud.reload();
    {
        let conn = state.db.lock().await;
        let _ = state
            .wordcloud
            .with_dict(|dict| wordcloud::sync_dict_file_version(&conn, dict));
    }
    let doc = state.wordcloud.snapshot_entities();
    let meta = state.wordcloud.entities_meta();
    Ok(Json(json!({
        "version": doc.version,
        "entities": doc.entities,
        "source": meta.source,
        "path": meta.path,
        "writable": meta.writable,
        "seedDir": meta.seed_dir,
        "cowDir": meta.cow_dir,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchEntityBody {
    pub canonical: Option<String>,
    pub aliases: Option<Vec<String>>,
}

/// `PATCH /api/wordcloud/entities/{id}` — update canonical (and optional aliases).
///
/// First edit against the shared seed copies entities into the papr COW dir.
/// Bumps the entities file version so term-index backfill picks up new display names.
pub async fn patch_entity(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<PatchEntityBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    if body.canonical.is_none() && body.aliases.is_none() {
        return Err(ApiError::bad_request("emptyPatch"));
    }
    let (ent, meta) = state
        .wordcloud
        .update_entity(
            &id,
            body.canonical.as_deref(),
            body.aliases,
        )
        .map_err(ApiError::from)?;
    let doc = state.wordcloud.snapshot_entities();
    let bumped = {
        let conn = state.db.lock().await;
        state
            .wordcloud
            .with_dict(|dict| wordcloud::sync_dict_file_version(&conn, dict))
            .map_err(ApiError::from)?
    };
    Ok(Json(json!({
        "ok": true,
        "entity": ent,
        "version": doc.version,
        "source": meta.source,
        "path": meta.path,
        "writable": meta.writable,
        "seedDir": meta.seed_dir,
        "cowDir": meta.cow_dir,
        "dictVersion": bumped.0,
        "dictBumped": bumped.1,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEntityBody {
    /// Optional stable id (`group.slug`). Auto-generated when omitted.
    pub id: Option<String>,
    pub canonical: String,
    pub group: Option<String>,
    pub aliases: Option<Vec<String>>,
}

/// `POST /api/wordcloud/entities` — create / promote a residual cloud term to an entity.
///
/// Use when a lowercase token like `ai` appears in the cloud but is not in the
/// gazetteer: create canonical `AI` (aliases optional; lowercase form is kept
/// automatically when casing differs). Cloud aggregation remaps stored
/// surfaces through the live gazetteer immediately; run term-index backfill
/// to rewrite `article_terms` for durable storage.
pub async fn create_entity(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateEntityBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let (ent, meta) = state
        .wordcloud
        .create_entity(
            body.id.as_deref(),
            &body.canonical,
            body.group.as_deref(),
            body.aliases,
        )
        .map_err(ApiError::from)?;
    let doc = state.wordcloud.snapshot_entities();
    let bumped = {
        let conn = state.db.lock().await;
        state
            .wordcloud
            .with_dict(|dict| wordcloud::sync_dict_file_version(&conn, dict))
            .map_err(ApiError::from)?
    };
    Ok(Json(json!({
        "ok": true,
        "entity": ent,
        "version": doc.version,
        "source": meta.source,
        "path": meta.path,
        "writable": meta.writable,
        "seedDir": meta.seed_dir,
        "cowDir": meta.cow_dir,
        "dictVersion": bumped.0,
        "dictBumped": bumped.1,
    })))
}

/// `GET /api/wordcloud/status` — term-index coverage (admin).
pub async fn status(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let st = wordcloud::backfill_status(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({
        "dictVersion": st.dict_version,
        "indexed": st.indexed,
        "stale": st.stale,
        "missing": st.missing,
        "totalArticles": st.total_articles,
    })))
}

#[derive(Deserialize)]
pub struct BackfillBody {
    /// Optional batch size (default 64, max 500). When set with `sync: true`,
    /// processes that many articles inline; otherwise kicks the background
    /// worker by ensuring dict version is set (worker drains the backlog).
    pub limit: Option<usize>,
    /// When true, process one batch in this request and return counts.
    #[serde(default)]
    pub sync: bool,
}

/// `POST /api/wordcloud/backfill` — (re)tokenize articles missing terms (admin).
///
/// With `sync: true`, runs one CPU+write batch in-request (tokenize happens
/// without holding the DB mutex for the whole batch). Otherwise returns
/// current status; the background job loop drains remaining work.
pub async fn backfill(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<BackfillBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let limit = body.limit.unwrap_or(wordcloud::BACKFILL_BATCH).clamp(1, 500);

    if body.sync {
        let (dict_version, rows) = {
            let conn = state.db.lock().await;
            let _ = wordcloud::ensure_dict_version(&conn);
            wordcloud::fetch_backfill_batch(&conn, limit).map_err(ApiError::from)?
        };
        let prepared = state
            .wordcloud
            .with_dict(|dict| wordcloud::tokenize_backfill_batch(&rows, dict));
        let result = {
            let conn = state.db.lock().await;
            wordcloud::write_backfill_batch(&conn, dict_version, &prepared)
                .map_err(ApiError::from)?;
            wordcloud::backfill_status(&conn).map_err(ApiError::from)?
        };
        return Ok(Json(json!({
            "ok": true,
            "sync": true,
            "processed": prepared.len(),
            "dictVersion": result.dict_version,
            "indexed": result.indexed,
            "stale": result.stale,
            "missing": result.missing,
            "totalArticles": result.total_articles,
            "remaining": result.missing + result.stale,
        })));
    }

    // Async mode: bump nothing — just report status; background loop works.
    let conn = state.db.lock().await;
    let _ = wordcloud::ensure_dict_version(&conn);
    let st = wordcloud::backfill_status(&conn).map_err(ApiError::from)?;
    Ok(Json(json!({
        "ok": true,
        "sync": false,
        "dictVersion": st.dict_version,
        "indexed": st.indexed,
        "stale": st.stale,
        "missing": st.missing,
        "totalArticles": st.total_articles,
        "remaining": st.missing + st.stale,
    })))
}
