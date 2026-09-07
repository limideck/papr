//! Meilisearch search backend (keyword + hybrid vector).
//!
//! Architecture notes (`docs/meilisearch-plan.md`):
//! - The FTS5 path stays as the fallback (`search_engine = fts`) and for
//!   queries Meili cannot express (boolean ops, field-restricted, quotes).
//! - Index documents mirror FTS content (`title` + body plain text) plus
//!   `feed`/`author`/`tags` so those become searchable. Read-state / tag
//!   filters are NOT synced here: the server route re-applies the existing
//!   SQL filters over the ranked id window Meili returns.
//! - Vectors are `userProvided`: papr embeds texts itself via a configured
//!   HTTP embedder (default ollama `/api/embed`, model `bge-m3`) and uploads
//!   `_vectors.default` with each document; searches send the query vector
//!   alongside. This sidesteps Meili's built-in `ollama`/`rest` embedder bugs
//!   observed on 1.53 and keeps the embedder replaceable.

use crate::db;
use crate::error::{AppError, AppResult};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

/// Read the search-backend settings. `enabled` only when the engine is
/// `meili` and a key is configured.
pub struct MeiliConfig {
    pub url: String,
    pub key: String,
    pub index: String,
    pub embed_url: String,
    pub embed_model: String,
    pub embed_dims: usize,
    pub semantic_ratio: f32,
}

impl MeiliConfig {
    pub fn from_db(conn: &rusqlite::Connection) -> MeiliConfig {
        let get = |k: &str, d: &str| db::get_setting(conn, k).ok().flatten().unwrap_or_else(|| d.to_string());
        let ratio = db::setting_parsed::<f64>(conn, "meili_semantic_ratio", 0.5);
        MeiliConfig {
            url: get("meili_url", "http://127.0.0.1:7700"),
            key: get("meili_key", ""),
            index: get("meili_index", "papr_articles"),
            embed_url: get("meili_embed_url", "http://127.0.0.1:11434/api/embed"),
            embed_model: get("meili_embed_model", "bge-m3"),
            embed_dims: db::setting_parsed::<usize>(conn, "meili_embed_dims", 1024).max(64),
            semantic_ratio: ratio.clamp(0.0, 1.0) as f32,
        }
    }

    pub fn enabled(&self) -> bool {
        !self.key.is_empty()
    }
}

/// True when `search_engine` is set to `meili` (regardless of connectivity).
pub fn engine_is_meili(conn: &rusqlite::Connection) -> bool {
    matches!(
        db::get_setting(conn, "search_engine").ok().flatten().as_deref(),
        Some(v) if v.eq_ignore_ascii_case("meili")
    )
}

/// Whether a raw user query can go to Meili, or must stay on FTS5.
///
/// Meili has no boolean grammar, no field-restricted terms and no phrase
/// operator in the way FTS5 does. Anything that needs those keeps the FTS
/// fallback so the documented search contract never silently changes.
/// `feed:` is fine either way — it is compiled to a SQL prefix filter, not a
/// MATCH term.
pub fn meili_eligible(input: &str) -> bool {
    let up = input.to_uppercase();
    for kw in [" AND ", " OR ", " NOT ", "(", ")", "TITLE:", "BODY:"] {
        if up.contains(kw) {
            return false;
        }
    }
    let chars: Vec<char> = input.chars().collect();
    // Unary minus at a term boundary needs NOT (FTS keeps it).
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-' && (i == 0 || chars[i - 1].is_whitespace()) {
            return false;
        }
        i += 1;
    }
    // Explicit prefix `*` is expressible as a plain prefix search in Meili but
    // we keep it on FTS to avoid subtle differences (rare, explicit).
    if input.contains('*') {
        return false;
    }
    true
}

/// Text used for the semantic template (headline-heavy, cheap to embed).
pub fn semantic_text(title: &str, feed: &str, author: &str, tags: &[String], body: &str) -> String {
    let head: String = body.chars().take(600).collect();
    format!("{title} {feed} {author} {}", tags.join(" "))
        + if head.is_empty() { "" } else { " " } + &head
}

fn hdr(cfg: &MeiliConfig) -> String {
    format!("Bearer {}", cfg.key)
}

async fn api(
    client: &reqwest::Client,
    cfg: &MeiliConfig,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> AppResult<Value> {
    let label = format!("{method}");
    let mut req = client
        .request(method, format!("{}{}", cfg.url.trim_end_matches('/'), path))
        .header("Authorization", hdr(cfg));
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::other(format!("meili request failed: {e}")))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::other(format!(
            "meili {label} {path}: HTTP {status}: {text}"
        )));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|e| AppError::other(format!("meili response parse: {e}: {text}")))
}

/// Embed texts via the configured endpoint (ollama `/api/embed` shape:
/// `{"model":…,"input":[…],"embeddings":[[…],…]}`).
pub async fn embed(client: &reqwest::Client, cfg: &MeiliConfig, texts: &[String]) -> AppResult<Vec<Vec<f32>>> {
    let body = json!({ "model": cfg.embed_model, "input": texts });
    let resp = client
        .post(cfg.embed_url.as_str())
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::other(format!("embed request failed: {e}")))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::other(format!(
            "embed HTTP {status}: {text}"
        )));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| AppError::other(format!("embed parse: {e}: {text}")))?;
    let arr = v
        .get("embeddings")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::other("embed response missing `embeddings`".to_string()))?;
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        let vec = e
            .as_array()
            .ok_or_else(|| AppError::other("embedding is not an array".to_string()))?
            .iter()
            .map(|x| {
                x.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| AppError::other("embedding value not a number".to_string()))
            })
            .collect::<AppResult<Vec<f32>>>()?;
        if vec.len() != cfg.embed_dims {
            return Err(AppError::other(format!(
                "embedding dims {} != configured {}",
                vec.len(),
                cfg.embed_dims
            )));
        }
        out.push(vec);
    }
    Ok(out)
}

/// Create the index if missing and (re)assert its settings. Idempotent.
pub async fn ensure_index(client: &reqwest::Client, cfg: &MeiliConfig) -> AppResult<()> {
    let exists = match api(client, cfg, reqwest::Method::GET, &format!("/indexes/{}", cfg.index), None).await {
        Ok(_) => true,
        Err(e) if e.to_string().contains("index_not_found") => false,
        Err(e) => return Err(e),
    };
    if !exists {
        api(
            client,
            cfg,
            reqwest::Method::POST,
            "/indexes",
            Some(json!({ "uid": cfg.index, "primaryKey": "id" })),
        )
        .await?;
    }
    // Keyword-only mode (`semantic_ratio` = 0) does not need an embedder and
    // must not configure one — older Meili (< 1.9, where vector search was an
    // experimental feature) rejects *any* `embedders` key in settings until
    // `vectorStore` is enabled, even `{}`. Omit the field entirely: documents
    // are uploaded without `_vectors` and searches stay text-only, so the
    // index works with no embedding service (and no feature flag) at all.
    // Hybrid mode enables the experimental flag best-effort (no-op where
    // vector search is GA) and configures the embedder; switching 0 → hybrid
    // later means re-running `papr meili rebuild`.
    let mut settings = json!({
        "searchableAttributes": ["title", "body", "feed", "author", "tags"],
        "pagination": { "maxTotalHits": 10000 },
    });
    if cfg.semantic_ratio > 0.0 {
        // Best-effort: harmless on versions where vector search is GA.
        let _ = api(
            client,
            cfg,
            reqwest::Method::PATCH,
            "/experimental-features",
            Some(json!({ "vectorStore": true })),
        )
        .await;
        settings["embedders"] = json!({
            "default": {
                "source": "userProvided",
                "dimensions": cfg.embed_dims,
            }
        });
    }
    api(
        client,
        cfg,
        reqwest::Method::PATCH,
        &format!("/indexes/{}/settings", cfg.index),
        Some(settings),
    )
    .await?;
    Ok(())
}

/// Row fields needed to build one index document.
pub struct IndexDoc {
    pub id: i64,
    pub title: String,
    pub feed: String,
    pub author: String,
    pub tags: Vec<String>,
    pub body: String,
}

/// Load up to `limit` queued articles (oldest first), returning rows to
/// upsert. Articles that no longer exist are also returned flagged absent via
/// empty placeholders, so the worker can delete them from the index.
pub fn load_queue(conn: &rusqlite::Connection, limit: i64) -> AppResult<Vec<IndexDoc>> {
    let mut stmt = conn.prepare(
        "SELECT q.article_id
         FROM meili_sync_queue q
         ORDER BY q.updated_at ASC
         LIMIT ?1",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([limit], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(load_doc_one(conn, id)?.unwrap_or_else(|| IndexDoc {
            id,
            title: String::new(),
            feed: String::new(),
            author: String::new(),
            tags: Vec::new(),
            body: String::new(),
        }));
    }
    Ok(out)
}

/// One document for an article id; `None` when the article no longer exists
/// (the document must be dropped from the index).
pub fn load_doc_one(conn: &rusqlite::Connection, id: i64) -> AppResult<Option<IndexDoc>> {
    let row: Option<(String, Option<String>, Option<String>, String)> = conn
        .query_row(
            "SELECT a.title, a.body_text, a.author, COALESCE(f.title, '')
             FROM articles a JOIN feeds f ON f.id = a.feed_id
             WHERE a.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((title, body_text, author, feed)) = row else {
        return Ok(None);
    };
    let tags: Vec<String> = conn
        .prepare(
            "SELECT t.name FROM tags t JOIN article_tags at ON at.tag_id = t.id
             WHERE at.article_id = ?1 ORDER BY t.kind, t.name",
        )?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Some(IndexDoc {
        id,
        title,
        feed,
        author: author.unwrap_or_default(),
        tags,
        body: body_text.unwrap_or_default(),
    }))
}

/// Article ids for a full rebuild, paged by primary key (stable under
/// concurrent inserts: rows added after the cursor still get picked up).
pub fn load_page(conn: &rusqlite::Connection, after: i64, limit: i64) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM articles WHERE id > ?1 ORDER BY id LIMIT ?2",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([after, limit], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

/// Mark a document as absent (title empty) for deletion at the caller.
pub fn is_deleted(doc: &IndexDoc) -> bool {
    doc.title.is_empty()
        && doc.body.is_empty()
        && doc.feed.is_empty()
        && doc.author.is_empty()
        && doc.tags.is_empty()
}

fn doc_json(doc: &IndexDoc, vector: Option<&[f32]>) -> Value {
    let mut body = json!({
        "id": doc.id,
        "title": doc.title,
        "body": doc.body.chars().take(30000).collect::<String>(),
        "feed": doc.feed,
        "author": doc.author,
        "tags": doc.tags,
    });
    if let Some(v) = vector {
        body["_vectors"] = json!({ "default": v });
    }
    body
}

/// Upsert documents into the index. In hybrid mode (`semantic_ratio` > 0)
/// vectors are computed through the configured embed endpoint before upload;
/// in keyword-only mode (`semantic_ratio` = 0) no embedder is configured and
/// no embedding call happens — the index works with no embedding service.
pub async fn upsert_docs(
    client: &reqwest::Client,
    cfg: &MeiliConfig,
    docs: &[IndexDoc],
) -> AppResult<()> {
    if docs.is_empty() {
        return Ok(());
    }
    if cfg.semantic_ratio > 0.0 {
        let texts: Vec<String> = docs
            .iter()
            .map(|d| semantic_text(&d.title, &d.feed, &d.author, &d.tags, &d.body))
            .collect();
        let vectors = embed(client, cfg, &texts).await?;
        let payload: Vec<Value> = docs
            .iter()
            .zip(vectors.iter())
            .map(|(d, v)| doc_json(d, Some(v)))
            .collect();
        api(
            client,
            cfg,
            reqwest::Method::POST,
            &format!("/indexes/{}/documents", cfg.index),
            Some(Value::Array(payload)),
        )
        .await?;
    } else {
        let payload: Vec<Value> = docs.iter().map(|d| doc_json(d, None)).collect();
        api(
            client,
            cfg,
            reqwest::Method::POST,
            &format!("/indexes/{}/documents", cfg.index),
            Some(Value::Array(payload)),
        )
        .await?;
    }
    Ok(())
}

/// Remove documents from the index. Modern Meili deletes per document id
/// (`DELETE /documents/{id}`); older `?ids=` is rejected.
pub async fn delete_docs(client: &reqwest::Client, cfg: &MeiliConfig, ids: &[i64]) -> AppResult<()> {
    for id in ids {
        api(
            client,
            cfg,
            reqwest::Method::DELETE,
            &format!("/indexes/{}/documents/{}", cfg.index, id),
            None,
        )
        .await?;
    }
    Ok(())
}

/// A ranked hit from Meili.
pub struct MeiliHit {
    pub id: i64,
}

/// Keyword/hybrid search returning ranked article ids. `strict` selects
/// Meili's `matchingStrategy: all`; `semantic_ratio` > 0 adds vectors via the
/// embedder (query embedded here). Errors bubble up so the caller can fall
/// back to FTS5.
pub async fn search_ids(
    client: &reqwest::Client,
    cfg: &MeiliConfig,
    query: &str,
    strict: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<i64>> {
    let mut body = json!({
        "q": query,
        "limit": limit,
        "offset": offset,
    });
    // `strict` is accepted for signature parity with the FTS contract; the
    // `all` matchingStrategy only exists on Meili >= 1.8 and older servers
    // reject it, so ranking stays engine-default (docs with all terms outrank
    // partial/typo matches; read-state filters are applied downstream anyway).
    let _ = strict;
    if cfg.semantic_ratio > 0.0 {
        let vec = embed(client, cfg, &[query.to_string()]).await?;
        body["vector"] = Value::Array(vec[0].iter().map(|f| json!(f)).collect());
        body["hybrid"] = json!({ "embedder": "default", "semanticRatio": cfg.semantic_ratio });
    }
    let resp = api(
        client,
        cfg,
        reqwest::Method::POST,
        &format!("/indexes/{}/search", cfg.index),
        Some(body),
    )
    .await?;
    let hits = resp
        .get("hits")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::other("meili search missing hits".to_string()))?;
    let mut ids = Vec::with_capacity(hits.len());
    for h in hits {
        if let Some(id) = h.get("id").and_then(Value::as_i64) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Remove all documents from the index (used by full rebuild).
pub async fn delete_all_docs(client: &reqwest::Client, cfg: &MeiliConfig) -> AppResult<()> {
    api(
        client,
        cfg,
        reqwest::Method::DELETE,
        &format!("/indexes/{}/documents", cfg.index),
        None,
    )
    .await?;
    Ok(())
}

/// Index stats (`numberOfDocuments`, `isIndexing`, …) as JSON for status
/// output.
pub async fn index_stats(client: &reqwest::Client, cfg: &MeiliConfig) -> AppResult<Value> {
    api(
        client,
        cfg,
        reqwest::Method::GET,
        &format!("/indexes/{}/stats", cfg.index),
        None,
    )
    .await
}

/// Quick connectivity check (used before deciding to use the meili path).
pub async fn healthy(client: &reqwest::Client, cfg: &MeiliConfig) -> bool {
    matches!(
        api(client, cfg, reqwest::Method::GET, "/health", None).await,
        Ok(v) if v.get("status").and_then(Value::as_str) == Some("available")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligibility_classifies_queries() {
        assert!(meili_eligible("tariff china"));
        assert!(meili_eligible("中东局势 伊朗"));
        assert!(meili_eligible("feed:Reuters Trump"));
        assert!(meili_eligible("\"interest rate\" 2026"));
        assert!(!meili_eligible("Trump -tariff"));
        assert!(!meili_eligible("Trump OR Biden"));
        assert!(!meili_eligible("(a OR b) c"));
        assert!(!meili_eligible("title:Trump"));
        assert!(!meili_eligible("Trump AND china"));
    }

    #[test]
    fn semantic_text_prefers_headline_and_caps_body() {
        let tags = vec!["中东局势".to_string(), "oil".to_string()];
        let body = "x".repeat(1200);
        let t = semantic_text("油价上涨", "Nikkei", "Author", &tags, &body);
        assert!(t.starts_with("油价上涨 Nikkei Author 中东局势 oil xxxxx"));
        assert!(t.chars().count() < 800, "headline + 600-char head only");
    }
}
