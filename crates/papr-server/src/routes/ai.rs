//! AI routes: streaming article summaries, RAG Q&A, digests, and full-text
//! translation over the web API.
//!
//! The desktop app drives these through Tauri commands; the web app reaches the
//! same `papr_core::ai` / `papr_core::translate` implementations over HTTP.
//! Each stream is delivered as SSE (`text/event-stream`), one JSON event per
//! frame, matching the shapes the frontend's `apiStream` consumer expects.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use papr_core::ai::{self, AiConfig, AiEvent};
use papr_core::db;
use papr_core::error::AppError;
use papr_core::sanitize;
use papr_core::translate::{self, TranslateEvent};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::convert::Infallible;
use tokio_stream::wrappers::ReceiverStream;

/// The provider/model to attribute in the ledger for an already-resolved
/// translation backend — empty for keyless machine-translation engines (which
/// report zero usage and are skipped by the db layer anyway).
fn backend_identity(backend: &translate::Backend) -> (&'static str, String) {
    match backend {
        translate::Backend::Llm(cfg) => (cfg.provider_name(), cfg.model().to_string()),
        _ => ("", String::new()),
    }
}

/// Build an SSE `data:` frame carrying `value` as JSON.
fn sse_frame<T: serde::Serialize>(value: &T) -> Event {
    Event::default().data(serde_json::to_string(value).unwrap_or_default())
}

/// Load the AI provider configuration from the settings table.
fn load_ai_config(conn: &Connection) -> ApiResult<AiConfig> {
    AiConfig::new(
        db::get_setting(conn, "ai_provider").map_err(ApiError::from)?,
        db::get_setting(conn, "ai_api_key").map_err(ApiError::from)?,
        db::get_setting(conn, "ai_model").map_err(ApiError::from)?,
        db::get_setting(conn, "ai_base_url").map_err(ApiError::from)?,
    )
    .map_err(ApiError::from)
}

/// Truncate to at most `max` characters without splitting a UTF-8 boundary.
fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A system-prompt directive so AI output matches the UI language rather than
/// defaulting to whatever language the source article happens to be in.
fn response_language(conn: &Connection) -> &'static str {
    match db::get_setting(conn, "language").ok().flatten().as_deref() {
        Some("zh") => "\n\nAlways write your response in Simplified Chinese.",
        Some("ja") => "\n\nAlways write your response in Japanese.",
        _ => "\n\nAlways write your response in English.",
    }
}

/// The article-translation target language code: the dedicated
/// `translate_target_lang` setting, falling back to the UI `language`, then
/// English.
fn translate_target_lang(conn: &Connection) -> String {
    db::get_setting(conn, "translate_target_lang")
        .ok()
        .flatten()
        .or_else(|| db::get_setting(conn, "language").ok().flatten())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "en".to_string())
}

/// Resolve a translation engine (`google` / `bing` / `deepl` / `llm`). Google,
/// DeepL and Bing are keyless free endpoints; only the LLM path needs the
/// shared AI provider config. Unknown engine strings are rejected loudly
/// instead of silently falling back to the (billable) LLM path — a typo or a
/// newly added engine name would otherwise burn DeepSeek tokens unnoticed.
fn build_translate_selection(conn: &Connection, engine: &str) -> ApiResult<translate::Selection> {
    Ok(match engine {
        "google" => translate::Selection::Google,
        "bing" => translate::Selection::Bing,
        "deepl" => translate::Selection::Deepl,
        "llm" => translate::Selection::Llm(load_ai_config(conn)?),
        _ => {
            return Err(ApiError::from(AppError::code("unknownTranslateEngine")));
        }
    })
}

/// Stream a single chat completion to the client as SSE token deltas.
///
/// The generation runs on a blocking task so `ai::stream_chat`'s synchronous
/// token sink can push into an `mpsc` channel with real backpressure; `on_done`
/// runs on completion (only when the stream ran to full completion) — used to
/// persist a finished summary rather than a truncated fragment.
async fn stream_chat_sse<F>(
    http: &reqwest::Client,
    cfg: AiConfig,
    system: String,
    user: String,
    max_tokens: u32,
    on_done: F,
) -> ApiResult<Response>
where
    F: FnOnce(ai::ChatOutcome) + Send + 'static,
{
    // The completion runs on a blocking thread. `Handle::block_on` enters the
    // runtime context on that thread, so a tokio `blocking_send` from the token
    // sink would panic ("Cannot block the current thread from within a
    // runtime"). The sink therefore pushes into a plain bounded `std` channel,
    // which a second blocking task forwards into a tokio channel the SSE layer
    // can poll. Dropping the SSE receiver (client disconnect) closes the chain:
    // the bridge gives up, the producer's `send` fails, and `stream_chat` stops.
    let (std_tx, std_rx) = std::sync::mpsc::sync_channel::<Result<Event, Infallible>>(128);
    let http = http.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let std_tx_delta = std_tx.clone();
        let mut sink = move |delta: &str| {
            std_tx_delta
                .send(Ok(sse_frame(&AiEvent::Delta(delta.to_string()))))
                .is_ok()
        };
        let outcome = handle.block_on(ai::stream_chat(
            &http, &cfg, &system, &user, &mut sink, max_tokens,
        ));
        match outcome {
            Ok(outcome) => {
                let _ = std_tx.send(Ok(sse_frame(&AiEvent::Done)));
                on_done(outcome);
            }
            Err(e) => {
                let _ = std_tx.send(Ok(sse_frame(&AiEvent::Error(e.to_string()))));
            }
        }
    });

    let (tokio_tx, tokio_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
    tokio::task::spawn_blocking(move || {
        while let Ok(item) = std_rx.recv() {
            if tokio_tx.blocking_send(item).is_err() {
                break;
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(tokio_rx)).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeBody {
    pub article_id: i64,
}

/// `POST /api/ai/summarize` — stream a TL;DR + bullets for one article and, on
/// completion, persist it.
pub async fn summarize(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<SummarizeBody>,
) -> ApiResult<Response> {
    let (title, article_body, cfg, lang, db, article_id) = {
        let conn = state.db.lock().await;
        let (title, article_body) = db::article_text(&conn, body.article_id).map_err(ApiError::from)?;
        (
            title,
            article_body,
            load_ai_config(&conn)?,
            response_language(&conn),
            state.db.clone(),
            body.article_id,
        )
    };
    if article_body.trim().is_empty() {
        return Err(ApiError::from(AppError::code("noArticleBody")));
    }
    let system = format!(
        "You are a sharp news editor. Summarize the article so a reader can \
         decide whether to read it in full.\n\n\
         Format the response in markdown using exactly this shape:\n\
         **TL;DR** — One sentence capturing the single most important point.\n\n\
         - Key fact, finding, or claim (under ~20 words)\n\
         - Another key point\n\
         - 3 to 5 bullets total, one idea each, no nested bullets\n\n\
         Output only this structure. No preamble, no closing remarks, no \
         section headers, no extra prose.{lang}"
    );
    let user = format!("Title: {title}\n\n{}", truncate(&article_body, 8000));
    let (provider, model) = (cfg.provider_name(), cfg.model().to_string());
    stream_chat_sse(&state.http, cfg, system, user, ai::MAX_TOKENS, move |outcome| {
        if outcome.completed {
            let conn = blocking_lock(&db);
            // Persist only a summary that streamed to completion, so a dropped
            // client (closed drawer) never caches a truncated half-summary.
            if !outcome.text.trim().is_empty() {
                let _ = db::set_ai_summary(&conn, article_id, outcome.text.trim());
            }
            let _ = db::record_ai_usage(
                &conn,
                "summarize",
                provider,
                &model,
                outcome.usage,
            );
        }
    })
    .await
}

/// Lock the shared DB connection from a blocking task context.
/// Uses `blocking_lock()` which is the idiomatic way to acquire a tokio Mutex
/// guard from a `spawn_blocking` thread (avoids nested `block_on`).
fn blocking_lock(
    db: &std::sync::Arc<tokio::sync::Mutex<Connection>>,
) -> tokio::sync::MutexGuard<'_, Connection> {
    db.blocking_lock()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskBody {
    pub question: String,
}

/// `POST /api/ai/ask` — answer a question using subscribed articles as RAG.
pub async fn ask(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<AskBody>,
) -> ApiResult<Response> {
    let (cfg, context, lang) = {
        let conn = state.db.lock().await;
        let cfg = load_ai_config(&conn)?;
        let hits = db::search_articles_for_rag(&conn, &body.question, 6).map_err(ApiError::from)?;
        let mut context = String::new();
        for (id, _title, feed_title) in hits {
            let (title, article_body) = db::article_text(&conn, id).map_err(ApiError::from)?;
            context.push_str(&format!(
                "## {} — {}\n{}\n\n",
                title,
                feed_title,
                truncate(&article_body, 1200)
            ));
        }
        (cfg, context, response_language(&conn))
    };
    let system = format!(
        "You answer the user's question using only the provided \
         articles from their RSS subscriptions. Cite the article \
         titles you draw from. If the articles do not contain the \
         answer, say so plainly.{lang}"
    );
    let user = if context.trim().is_empty() {
        format!("No relevant articles were found.\n\nQuestion: {}", body.question)
    } else {
        format!(
            "Articles from the user's feeds:\n\n{context}---\n\nQuestion: {}",
            body.question
        )
    };
    let (provider, model) = (cfg.provider_name(), cfg.model().to_string());
    let db = state.db.clone();
    stream_chat_sse(&state.http, cfg, system, user, ai::MAX_TOKENS, move |outcome| {
        if outcome.completed {
            let conn = blocking_lock(&db);
            let _ = db::record_ai_usage(
                &conn,
                "ask",
                provider,
                &model,
                outcome.usage,
            );
        }
    })
    .await
}

/// `POST /api/ai/digest` — synthesize a briefing of the most recent articles.
/// Serves a cached briefing within the TTL instead of regenerating it.
pub async fn digest(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(_): Json<Value>,
) -> ApiResult<Response> {
    // Cache hit: replay the stored briefing as the same SSE stream a fresh
    // generation would emit, spending zero LLM tokens.
    let cached = {
        let conn = state.db.lock().await;
        db::get_digest_cache(&conn).map_err(ApiError::from)?
    };
    if let Some(text) = cached {
        return Ok(cached_digest_sse(&text));
    }
    let (cfg, articles, lang) = {
        let conn = state.db.lock().await;
        (
            load_ai_config(&conn)?,
            db::digest_source(&conn, 30).map_err(ApiError::from)?,
            response_language(&conn),
        )
    };
    if articles.is_empty() {
        return Err(ApiError::from(AppError::code("noArticles")));
    }
    let mut corpus = String::new();
    for (title, feed, text) in &articles {
        corpus.push_str(&format!("- [{feed}] {title}: {}\n", truncate(text, 400)));
    }
    let system = format!(
        "You are the user's personal news briefer. From the recent \
         articles, write a crisp briefing: group related items into \
         2-4 themed sections with short headers, lead with what \
         matters most, and keep it skimmable. Plain prose, no preamble.{lang}"
    );
    let user = format!("Recent articles from my feeds:\n\n{corpus}");
    let (provider, model) = (cfg.provider_name(), cfg.model().to_string());
    let db = state.db.clone();
    stream_chat_sse(&state.http, cfg, system, user, ai::DIGEST_MAX_TOKENS, move |outcome| {
        if outcome.completed {
            let conn = blocking_lock(&db);
            let _ = db::record_ai_usage(
                &conn,
                "digest",
                provider,
                &model,
                outcome.usage,
            );
            // Persist only a completed, non-empty briefing so a truncated
            // fragment (dropped client) is never cached.
            if !outcome.text.trim().is_empty() {
                let _ = db::set_digest_cache(&conn, outcome.text.trim());
            }
        }
    })
    .await
}

/// Replay a cached digest as the same SSE stream a fresh generation produces —
/// one `Delta` carrying the whole briefing, then `Done` — so the client needs
/// no cache-aware branch.
fn cached_digest_sse(text: &str) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(4);
    let text = text.to_string();
    tokio::spawn(async move {
        let _ = tx.send(Ok(sse_frame(&AiEvent::Delta(text)))).await;
        let _ = tx.send(Ok(sse_frame(&AiEvent::Done))).await;
    });
    Sse::new(ReceiverStream::new(rx)).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateBody {
    pub article_id: i64,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub engine: String,
}

/// `POST /api/ai/translate` — translate one article body, streaming per-batch
/// progress, and cache the result on completion.
///
/// Runs as a normal async task (matching the Tauri command). An earlier
/// `spawn_blocking` + `Handle::block_on` + `mpsc::blocking_send` path panicked
/// with "Cannot block the current thread from within a runtime" on the first
/// batch event — the SSE response had already returned 200, so the client saw
/// a silent empty "translation" with no toast.
pub async fn translate(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<TranslateBody>,
) -> ApiResult<Response> {
    let engine = if body.engine.trim().is_empty() {
        "llm".to_string()
    } else {
        body.engine.clone()
    };
    let (source_html, sel, target, db, article_id) = {
        let conn = state.db.lock().await;
        let detail = db::get_article(&conn, body.article_id).map_err(ApiError::from)?;
        let source = detail
            .extracted_html
            .filter(|s| !s.trim().is_empty())
            .or(detail.content_html)
            .unwrap_or_default();
        let target = if body.lang.trim().is_empty() {
            translate_target_lang(&conn)
        } else {
            body.lang.clone()
        };
        (
            source,
            build_translate_selection(&conn, &engine)?,
            target,
            state.db.clone(),
            body.article_id,
        )
    };
    if source_html.trim().is_empty() {
        return Err(ApiError::from(AppError::code("noArticleBody")));
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let http = state.http.clone();
    tokio::spawn(async move {
        let result: Result<String, AppError> = async {
            let backend = translate::ready(&http, sel).await?;
            let (provider, model) = backend_identity(&backend);
            let batches = translate::chunk_blocks(&source_html, translate::chunk_budget(&engine));
            let total = batches.len();
            let _ = tx
                .send(Ok(sse_frame(&TranslateEvent::Start { total })))
                .await;
            let system = translate::translate_system_prompt(translate::language_name(&target));
            let mut full = String::new();
            let mut usage = ai::TokenUsage::default();
            for (i, batch) in batches.iter().enumerate() {
                let raw = backend
                    .translate_batch(&http, &system, batch, &target, &mut usage)
                    .await?;
                let clean = sanitize::sanitize(raw.trim(), None);
                full.push_str(&clean);
                full.push('\n');
                let _ = tx
                    .send(Ok(sse_frame(&TranslateEvent::Batch {
                        html: clean,
                        done: i + 1,
                    })))
                    .await;
            }
            let final_html = full.trim().to_string();
            if !final_html.is_empty() {
                let conn = db.lock().await;
                let _ = db::set_translation(&conn, article_id, &final_html, &target);
                let _ = db::record_ai_usage(
                    &conn,
                    "translate",
                    provider,
                    &model,
                    usage,
                );
            }
            Ok(final_html)
        }
        .await;
        match result {
            Ok(final_html) => {
                let _ = tx
                    .send(Ok(sse_frame(&TranslateEvent::Done { html: final_html })))
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(Ok(sse_frame(&TranslateEvent::Error(e.to_string()))))
                    .await;
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx)).into_response())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticlePreviewTranslation {
    article_id: i64,
    title: String,
    snippet: String,
    lang: String,
    engine: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewBody {
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub engine: String,
}

fn escape_preview_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn preview_translation_html(title: &str, snippet: &str) -> String {
    format!(
        "<h1>{}</h1><p>{}</p>",
        escape_preview_text(title),
        escape_preview_text(snippet)
    )
}

fn text_for_selector(fragment: &str, selector: &str) -> String {
    let doc = scraper::Html::parse_fragment(fragment);
    let selector = scraper::Selector::parse(selector).expect("static preview selector");
    doc.select(&selector)
        .next()
        .map(|el| el.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `POST /api/articles/{id}/translate-preview` — translate the lightweight
/// title + body snippet the article list shows.
pub async fn translate_preview(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(article_id): Path<i64>,
    Json(body): Json<PreviewBody>,
) -> ApiResult<Json<ArticlePreviewTranslation>> {
    let engine = if body.engine.trim().is_empty() {
        "llm".to_string()
    } else {
        body.engine
    };
    let (title, snippet, sel, target) = {
        let conn = state.db.lock().await;
        let target = if body.lang.trim().is_empty() {
            translate_target_lang(&conn)
        } else {
            body.lang.clone()
        };
        if let Some((title, snippet)) =
            db::get_preview_translation(&conn, article_id, &target, &engine)?
        {
            return Ok(Json(ArticlePreviewTranslation {
                article_id,
                title,
                snippet,
                lang: target,
                engine,
            }));
        }
        let (title, article_body) = db::article_preview_text(&conn, article_id)?;
        (
            title,
            truncate(article_body.trim(), db::PREVIEW_SNIPPET_CHARS),
            build_translate_selection(&conn, &engine)?,
            target,
        )
    };
    if title.trim().is_empty() && snippet.trim().is_empty() {
        return Err(ApiError::from(AppError::code("noArticleBody")));
    }
    // Resolve the chosen engine (fetching Bing's auth token, if selected) before
    // translating, so a credential or network failure surfaces before any work.
    let backend = translate::ready(&state.http, sel)
        .await
        .map_err(ApiError::from)?;
    let (provider, model) = backend_identity(&backend);
    let system = translate::translate_system_prompt(translate::language_name(&target));
    let source = preview_translation_html(&title, &snippet);
    let mut usage = ai::TokenUsage::default();
    let raw = backend
        .translate_batch(&state.http, &system, &source, &target, &mut usage)
        .await
        .map_err(ApiError::from)?;
    let clean = sanitize::sanitize(raw.trim(), None);
    let translated_title = text_for_selector(&clean, "h1");
    let translated_snippet = text_for_selector(&clean, "p");
    if translated_title.trim().is_empty() && translated_snippet.trim().is_empty() {
        return Err(ApiError::from(AppError::code("emptyPreviewTranslation")));
    }
    {
        let conn = state.db.lock().await;
        let (current_title, current_body) =
            db::article_preview_text(&conn, article_id).map_err(ApiError::from)?;
        let current_snippet = truncate(current_body.trim(), db::PREVIEW_SNIPPET_CHARS);
        if current_title != title || current_snippet != snippet {
            return Err(ApiError::from(AppError::code("articlePreviewChanged")));
        }
        db::set_preview_translation(
            &conn,
            article_id,
            &translated_title,
            &translated_snippet,
            &target,
            &engine,
        )
        .map_err(ApiError::from)?;
        let _ = db::record_ai_usage(
            &conn,
            "translate-preview",
            provider,
            &model,
            usage,
        );
    }
    Ok(Json(ArticlePreviewTranslation {
        article_id,
        title: translated_title,
        snippet: translated_snippet,
        lang: target,
        engine,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageQuery {
    #[serde(default = "default_usage_days")]
    pub days: i64,
}

fn default_usage_days() -> i64 {
    30
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageReport {
    #[serde(flatten)]
    stats: db::AiUsageStats,
    /// Estimated cost in CNY over the window, from DeepSeek-style
    /// per-million-token prices (defaults: deepseek-v4-flash official rates).
    estimated_cost: f64,
}

/// `GET /api/ai/usage?days=30` — AI token usage aggregated over a trailing
/// window, bucketed by feature, with a cost estimate from the current prices.
pub async fn usage(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<UsageQuery>,
) -> ApiResult<Json<AiUsageReport>> {
    let conn = state.db.lock().await;
    let stats = db::ai_usage_stats(&conn, q.days).map_err(ApiError::from)?;
    // DeepSeek-style CNY estimate: cache-hit + cache-miss + output.
    // Defaults are official deepseek-v4-flash prices.
    let estimated_cost = db::estimate_ai_cost_cny(&conn, &stats);
    Ok(Json(AiUsageReport { stats, estimated_cost }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use papr_core::translate;

    fn test_conn() -> rusqlite::Connection {
        let path = std::env::temp_dir().join(format!(
            "papr-ai-route-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        papr_core::db::open(&path).unwrap()
    }

    #[test]
    fn translate_selection_known_engines_and_rejects_unknown() {
        let conn = test_conn();
        // The llm branch needs provider config; seed minimal settings.
        for (k, v) in [
            ("ai_provider", "deepseek"),
            ("ai_api_key", "sk-test"),
            ("ai_model", "deepseek-v4-flash"),
            ("ai_base_url", "https://api.deepseek.com"),
        ] {
            papr_core::db::set_setting(&conn, k, v).unwrap();
        }

        assert!(matches!(
            build_translate_selection(&conn, "google").unwrap(),
            translate::Selection::Google
        ));
        assert!(matches!(
            build_translate_selection(&conn, "bing").unwrap(),
            translate::Selection::Bing
        ));
        assert!(matches!(
            build_translate_selection(&conn, "deepl").unwrap(),
            translate::Selection::Deepl
        ));
        assert!(matches!(
            build_translate_selection(&conn, "llm").unwrap(),
            translate::Selection::Llm(_)
        ));

        // Regression: unknown engine strings must NOT silently fall back to
        // the billable LLM path.
        for bad in ["baidu", ""] {
            match build_translate_selection(&conn, bad) {
                Ok(_) => panic!("unknown engine {bad:?} must be rejected, not routed to LLM"),
                Err(e) => assert!(
                    matches!(e.error, papr_core::error::AppError::Coded("unknownTranslateEngine")),
                    "unexpected error for {bad:?}"
                ),
            }
        }
    }
}
