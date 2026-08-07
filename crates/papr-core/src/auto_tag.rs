//! Auto-tag articles from title + summary via the configured LLM.
//!
//! Two taxonomies share the queue:
//! - **Interest** (`kind=interest`): closed vocabulary — the model may only
//!   choose from admin-defined interest tags. Unknown names are dropped.
//! - **AI** (`kind=ai`): free-form — the model may invent tags; they are
//!   created/attached as AI tags and never appear in the interest list.

use crate::ai::{self, AiConfig, ChatOutcome};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{TAG_KIND_AI, TAG_KIND_INTEREST};
use rusqlite::Connection;
use serde::Deserialize;

/// Max characters for a stored tag name (after trim).
pub const MAX_TAG_NAME_CHARS: usize = 32;

/// Default: at most this many tags of one kind attached to one article.
pub const DEFAULT_MAX_TAGS_PER_ARTICLE: i64 = 5;

/// Failed jobs stop retrying after this many attempts.
pub const MAX_ATTEMPTS: i64 = 3;

/// Output token cap for tagging. Tag JSON is tiny; keep cost/latency low.
pub const TAG_MAX_TOKENS: u32 = 128;

/// Record a completed auto-tag call in the usage ledger. Only meaningful for
/// LLM tagging — the queue is only ever populated for that path, and the db
/// layer skips zero-token rows anyway.
fn record_usage(
    conn: &Connection,
    cfg: &AiConfig,
    feature: &str,
    usage: ai::TokenUsage,
) {
    let _ = db::record_ai_usage(
        conn,
        feature,
        cfg.provider_name(),
        cfg.model(),
        usage,
    );
}

/// Trim, length-cap, and reject empty / pure-symbol names.
pub fn normalize_tag_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let capped: String = trimmed.chars().take(MAX_TAG_NAME_CHARS).collect();
    let capped = capped.trim();
    if capped.is_empty() {
        return None;
    }
    // Require at least one letter or digit so "---" / "!!!" don't become tags.
    if !capped.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    Some(capped.to_string())
}

#[derive(Debug, Deserialize)]
struct InterestTagPayload {
    #[serde(default)]
    tags: Vec<String>,
    /// Optional explicit "new" list; treated as suggestions and still filtered
    /// against the closed vocabulary (never created).
    #[serde(default)]
    new: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AiTagPayload {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    ai: Vec<String>,
    #[serde(default)]
    new: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CombinedTagPayload {
    #[serde(default)]
    interest: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    ai: Vec<String>,
}

/// Pull a JSON object out of model output (tolerates markdown fences / prose).
/// Public for unit tests.
pub fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&text[start..=end])
}

/// How model text maps to tags before apply.
#[derive(Debug, PartialEq, Eq)]
pub enum JsonParseOutcome<T> {
    /// Parsed successfully (may be an empty tag list).
    Ok(T),
    /// Empty / no `{...}` — soft-skip (do not burn retries as hard failures).
    SoftEmpty,
    /// Found braces but JSON did not deserialize — candidate for one repair.
    Invalid(String),
}

fn soft_or_object(text: &str) -> JsonParseOutcome<&str> {
    let trimmed = text.trim();
    // Empty / prose with no `{` → soft-done (empty tags). A bare `[...]` is
    // handled by `parse_string_array_fallback` in the interest/AI parsers.
    if trimmed.is_empty() || !trimmed.contains('{') {
        return JsonParseOutcome::SoftEmpty;
    }
    match extract_json_object(trimmed) {
        Some(s) => JsonParseOutcome::Ok(s),
        // Saw `{` but no complete `{...}` — repair candidate, not soft-skip.
        None => JsonParseOutcome::Invalid(
            "no complete JSON object in model response".into(),
        ),
    }
}

/// Bare `["a","b"]` when the model skipped the object wrapper.
fn parse_string_array_fallback(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    let start = trimmed.find('[')?;
    let end = trimmed.rfind(']')?;
    if end <= start {
        return None;
    }
    let arr = &trimmed[start..=end];
    let names: Vec<String> = serde_json::from_str(arr).ok()?;
    Some(dedupe_names(names))
}

fn dedupe_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        if let Some(n) = normalize_tag_name(&name) {
            if !out.iter().any(|e: &String| e.eq_ignore_ascii_case(&n)) {
                out.push(n);
            }
        }
    }
    out
}

/// Parse closed-vocabulary interest suggestions from model output.
pub fn parse_interest_payload_outcome(text: &str) -> JsonParseOutcome<Vec<String>> {
    match soft_or_object(text) {
        JsonParseOutcome::Ok(json_str) => match serde_json::from_str::<InterestTagPayload>(json_str)
        {
            Ok(payload) => {
                JsonParseOutcome::Ok(dedupe_names(payload.tags.into_iter().chain(payload.new)))
            }
            Err(e) => JsonParseOutcome::Invalid(e.to_string()),
        },
        JsonParseOutcome::SoftEmpty => {
            if let Some(names) = parse_string_array_fallback(text) {
                JsonParseOutcome::Ok(names)
            } else {
                JsonParseOutcome::SoftEmpty
            }
        }
        JsonParseOutcome::Invalid(e) => JsonParseOutcome::Invalid(e),
    }
}

/// Parse free-form AI tag suggestions from model output.
pub fn parse_ai_payload_outcome(text: &str) -> JsonParseOutcome<Vec<String>> {
    match soft_or_object(text) {
        JsonParseOutcome::Ok(json_str) => match serde_json::from_str::<AiTagPayload>(json_str) {
            Ok(payload) => JsonParseOutcome::Ok(dedupe_names(
                payload
                    .tags
                    .into_iter()
                    .chain(payload.ai)
                    .chain(payload.new),
            )),
            Err(e) => JsonParseOutcome::Invalid(e.to_string()),
        },
        JsonParseOutcome::SoftEmpty => {
            if let Some(names) = parse_string_array_fallback(text) {
                JsonParseOutcome::Ok(names)
            } else {
                JsonParseOutcome::SoftEmpty
            }
        }
        JsonParseOutcome::Invalid(e) => JsonParseOutcome::Invalid(e),
    }
}

/// Parse a combined interest + AI response.
pub fn parse_combined_payload_outcome(
    text: &str,
) -> JsonParseOutcome<(Vec<String>, Vec<String>)> {
    let json_str = match soft_or_object(text) {
        JsonParseOutcome::Ok(s) => s,
        JsonParseOutcome::SoftEmpty => return JsonParseOutcome::SoftEmpty,
        JsonParseOutcome::Invalid(e) => return JsonParseOutcome::Invalid(e),
    };
    match serde_json::from_str::<CombinedTagPayload>(json_str) {
        Ok(payload) => {
            let interest = dedupe_names(payload.interest);
            let ai = dedupe_names(payload.tags.into_iter().chain(payload.ai));
            JsonParseOutcome::Ok((interest, ai))
        }
        Err(e) => JsonParseOutcome::Invalid(e.to_string()),
    }
}

/// Parse closed-vocabulary interest suggestions from model output.
/// Empty / non-JSON responses yield an empty list (soft success).
pub fn parse_interest_payload(text: &str) -> AppResult<Vec<String>> {
    match parse_interest_payload_outcome(text) {
        JsonParseOutcome::Ok(v) => Ok(v),
        JsonParseOutcome::SoftEmpty => Ok(Vec::new()),
        JsonParseOutcome::Invalid(e) => Err(AppError::other(format!("auto-tag: invalid JSON: {e}"))),
    }
}

/// Parse free-form AI tag suggestions from model output.
pub fn parse_ai_payload(text: &str) -> AppResult<Vec<String>> {
    match parse_ai_payload_outcome(text) {
        JsonParseOutcome::Ok(v) => Ok(v),
        JsonParseOutcome::SoftEmpty => Ok(Vec::new()),
        JsonParseOutcome::Invalid(e) => Err(AppError::other(format!("auto-tag: invalid JSON: {e}"))),
    }
}

/// Parse a combined interest + AI response.
pub fn parse_combined_payload(text: &str) -> AppResult<(Vec<String>, Vec<String>)> {
    match parse_combined_payload_outcome(text) {
        JsonParseOutcome::Ok(v) => Ok(v),
        JsonParseOutcome::SoftEmpty => Ok((Vec::new(), Vec::new())),
        JsonParseOutcome::Invalid(e) => Err(AppError::other(format!("auto-tag: invalid JSON: {e}"))),
    }
}

/// Legacy alias used by older tests / call sites — interest closed-vocab parse.
pub fn parse_tag_payload(text: &str) -> AppResult<Vec<String>> {
    parse_interest_payload(text)
}

pub fn prompt_interest(title: &str, summary: &str, existing: &[String]) -> (String, String) {
    let system = "You tag news articles for an RSS reader. \
Reply with ONLY a JSON object, no markdown, no commentary. \
Format: {\"tags\":[\"name1\",\"name2\"]}. \
Choose ONLY from the existing-tags list (exact spelling). \
Never invent or propose new tag names. \
Use at most 5 tags. \
An empty tags array is fine when nothing in the list is relevant.";
    let existing_list = if existing.is_empty() {
        "(none yet)".to_string()
    } else {
        existing.join(", ")
    };
    let summary = truncate_summary(summary);
    let user = format!(
        "Existing tags: {existing_list}\n\nTitle: {title}\n\nSummary: {summary}"
    );
    (system.to_string(), user)
}

pub fn prompt_ai(title: &str, summary: &str, existing_ai: &[String]) -> (String, String) {
    let system = "You tag news articles for an RSS reader. \
Reply with ONLY a JSON object, no markdown, no commentary. \
Format: {\"tags\":[\"name1\",\"name2\"]}. \
Suggest short topical tags (1-3 words each) from the title and summary. \
Prefer reusing names from the existing-tags list when they fit (exact spelling). \
You may invent new tag names when nothing suitable exists. \
Use at most 5 tags. \
An empty tags array is fine when nothing useful applies.";
    let existing_list = if existing_ai.is_empty() {
        "(none yet)".to_string()
    } else {
        existing_ai.join(", ")
    };
    let summary = truncate_summary(summary);
    let user = format!(
        "Existing tags: {existing_list}\n\nTitle: {title}\n\nSummary: {summary}"
    );
    (system.to_string(), user)
}

pub fn prompt_combined(
    title: &str,
    summary: &str,
    interest: &[String],
    existing_ai: &[String],
) -> (String, String) {
    let system = "You tag news articles for an RSS reader. \
Reply with ONLY a JSON object, no markdown, no commentary. \
Format: {\"interest\":[\"name1\"],\"tags\":[\"name2\"]}. \
For \"interest\": choose ONLY from the interest-tags list (exact spelling); never invent. \
For \"tags\": short topical free-form labels (1-3 words); prefer existing AI tags when they fit; new names are allowed. \
Use at most 5 names in each array. Empty arrays are fine.";
    let interest_list = if interest.is_empty() {
        "(none yet)".to_string()
    } else {
        interest.join(", ")
    };
    let ai_list = if existing_ai.is_empty() {
        "(none yet)".to_string()
    } else {
        existing_ai.join(", ")
    };
    let summary = truncate_summary(summary);
    let user = format!(
        "Interest tags: {interest_list}\nExisting AI tags: {ai_list}\n\nTitle: {title}\n\nSummary: {summary}"
    );
    (system.to_string(), user)
}

/// Back-compat alias for the closed-vocab interest prompt.
pub fn prompt_for(title: &str, summary: &str, existing: &[String]) -> (String, String) {
    prompt_interest(title, summary, existing)
}

fn truncate_summary(summary: &str) -> String {
    let s = summary.trim();
    if s.is_empty() {
        "(no summary)".to_string()
    } else {
        s.chars().take(800).collect()
    }
}

/// Title + summary (or body snippet fallback) for one article.
pub fn load_article_text(conn: &Connection, article_id: i64) -> AppResult<(String, String)> {
    Ok(conn.query_row(
        "SELECT title,
                COALESCE(
                    NULLIF(trim(summary), ''),
                    substr(body_text, 1, 800),
                    ''
                )
         FROM articles WHERE id = ?1",
        rusqlite::params![article_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Attach only suggested tags that case-insensitively match an existing name
/// of the given kind. Unknown names are dropped; nothing is created.
pub fn apply_suggested_tags(
    conn: &Connection,
    article_id: i64,
    suggested: &[String],
    existing_names: &[String],
    max_total: i64,
    kind: &str,
) -> AppResult<()> {
    let kind = db::normalize_tag_kind(kind)?;
    let already = db::article_tag_count(conn, article_id, kind)?;
    let mut slots = (max_total - already).max(0);

    for name in suggested {
        if slots <= 0 {
            break;
        }
        let Some(name) = normalize_tag_name(name) else {
            continue;
        };
        let Some(canonical) = existing_names
            .iter()
            .find(|e| e.eq_ignore_ascii_case(&name))
            .cloned()
        else {
            continue;
        };

        // Known name only — create_tag is find-or-create within the kind and
        // will resolve to the existing row, never invent a new vocabulary entry
        // when callers only pass matched names.
        let tag_id = db::create_tag(conn, &canonical, kind)?;
        db::set_article_tag(conn, article_id, tag_id, true)?;
        slots -= 1;
    }
    Ok(())
}

/// Create-or-attach free-form AI tags from suggestions.
pub fn apply_ai_tags(
    conn: &Connection,
    article_id: i64,
    suggested: &[String],
    max_total: i64,
) -> AppResult<()> {
    let already = db::article_tag_count(conn, article_id, TAG_KIND_AI)?;
    let mut slots = (max_total - already).max(0);

    for name in suggested {
        if slots <= 0 {
            break;
        }
        let Some(name) = normalize_tag_name(name) else {
            continue;
        };
        let tag_id = db::create_tag(conn, &name, TAG_KIND_AI)?;
        db::set_article_tag(conn, article_id, tag_id, true)?;
        slots -= 1;
    }
    Ok(())
}

struct JobContext {
    title: String,
    summary: String,
    interest_names: Vec<String>,
    ai_names: Vec<String>,
    interest_on: bool,
    ai_on: bool,
    interest_max: i64,
    ai_max: i64,
    interest_count: i64,
    ai_count: i64,
    cfg: AiConfig,
}

fn load_job_context(conn: &Connection, article_id: i64) -> AppResult<JobContext> {
    let interest_on = db::setting_flag(conn, "auto_tag_enabled", false);
    let ai_on = db::setting_flag(conn, "ai_tag_enabled", false);
    if !interest_on && !ai_on {
        return Err(AppError::code("autoTagDisabled"));
    }
    let interest_max =
        db::setting_parsed(conn, "auto_tag_max_tags_per_article", DEFAULT_MAX_TAGS_PER_ARTICLE)
            .max(0);
    let ai_max =
        db::setting_parsed(conn, "ai_tag_max_tags_per_article", DEFAULT_MAX_TAGS_PER_ARTICLE).max(0);
    let interest_count = db::article_tag_count(conn, article_id, TAG_KIND_INTEREST)?;
    let ai_count = db::article_tag_count(conn, article_id, TAG_KIND_AI)?;
    let (title, summary) = load_article_text(conn, article_id)?;
    let interest_names: Vec<String> = db::list_tags(conn, Some(TAG_KIND_INTEREST))?
        .into_iter()
        .map(|t| t.name)
        .collect();
    let ai_names: Vec<String> = db::list_tags(conn, Some(TAG_KIND_AI))?
        .into_iter()
        .map(|t| t.name)
        .collect();
    let cfg = AiConfig::new(
        db::get_setting(conn, "ai_provider")?,
        db::get_setting(conn, "ai_api_key")?,
        db::get_setting(conn, "ai_model")?,
        db::get_setting(conn, "ai_base_url")?,
    )?;
    Ok(JobContext {
        title,
        summary,
        interest_names,
        ai_names,
        interest_on,
        ai_on,
        interest_max,
        ai_max,
        interest_count,
        ai_count,
        cfg,
    })
}

/// True when every enabled taxonomy is already at its per-article cap.
fn at_tag_caps(ctx: &JobContext, run_interest: bool, run_ai: bool) -> bool {
    let interest_full = !run_interest || ctx.interest_count >= ctx.interest_max;
    let ai_full = !run_ai || ctx.ai_count >= ctx.ai_max;
    interest_full && ai_full
}

fn record_outcome(conn: &Connection, cfg: &AiConfig, outcome: &ChatOutcome) {
    record_usage(conn, cfg, "auto-tag", outcome.usage);
}

async fn complete_tag_json(
    client: &reqwest::Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
) -> AppResult<ChatOutcome> {
    ai::complete_chat_json(client, cfg, system, user, TAG_MAX_TOKENS).await
}

/// One repair call: ask the model to reply with JSON only.
async fn repair_tag_json(
    client: &reqwest::Client,
    cfg: &AiConfig,
    system: &str,
    previous: &str,
) -> AppResult<ChatOutcome> {
    let snippet: String = previous.chars().take(240).collect();
    let user = format!(
        "Your previous reply was not valid JSON. Reply with JSON only (no markdown, no commentary).\n\
Previous output:\n{snippet}"
    );
    ai::complete_chat_json(client, cfg, system, &user, TAG_MAX_TOKENS).await
}

async fn tags_from_model<T, F>(
    client: &reqwest::Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    parse: F,
    soft_empty: T,
) -> AppResult<(T, ChatOutcome)>
where
    F: Fn(&str) -> JsonParseOutcome<T>,
    T: Clone,
{
    let mut outcome = complete_tag_json(client, cfg, system, user).await?;
    match parse(&outcome.text) {
        JsonParseOutcome::Ok(v) => Ok((v, outcome)),
        JsonParseOutcome::SoftEmpty => Ok((soft_empty, outcome)),
        JsonParseOutcome::Invalid(_) => {
            let repaired = repair_tag_json(client, cfg, system, &outcome.text).await?;
            // Accumulate usage from both calls.
            outcome.usage += repaired.usage;
            let text = repaired.text;
            match parse(&text) {
                JsonParseOutcome::Ok(v) => {
                    outcome.text = text;
                    Ok((v, outcome))
                }
                // Soft-done after repair: empty tags beat an endless failed pile.
                JsonParseOutcome::SoftEmpty | JsonParseOutcome::Invalid(_) => {
                    outcome.text = text;
                    Ok((soft_empty, outcome))
                }
            }
        }
    }
}

/// Run one auto-tag job end-to-end (caller holds no DB lock across the AI call).
///
/// Steps: load context → skip if at caps → LLM (+ one JSON repair) → apply tags.
/// Admin backfill force-reenqueues past the recent-done guard; at-cap skip still
/// applies because apply_* cannot attach more tags past the configured max.
pub async fn process_article(
    db: &tokio::sync::Mutex<Connection>,
    client: &reqwest::Client,
    article_id: i64,
) -> AppResult<()> {
    let ctx = {
        let conn = db.lock().await;
        load_job_context(&conn, article_id)?
    };

    // Closed interest vocab with nothing configured: skip that path only.
    let run_interest = ctx.interest_on && !ctx.interest_names.is_empty();
    let run_ai = ctx.ai_on;

    if !run_interest && !run_ai {
        // Interest enabled but empty vocab, AI off — nothing to do.
        return Ok(());
    }

    // Skip LLM when every enabled kind is already at its cap.
    if at_tag_caps(&ctx, run_interest, run_ai) {
        return Ok(());
    }

    if run_interest && run_ai {
        let (system, user) = prompt_combined(
            &ctx.title,
            &ctx.summary,
            &ctx.interest_names,
            &ctx.ai_names,
        );
        let ((interest, ai_tags), outcome) = tags_from_model(
            client,
            &ctx.cfg,
            &system,
            &user,
            parse_combined_payload_outcome,
            (Vec::new(), Vec::new()),
        )
        .await?;
        let conn = db.lock().await;
        record_outcome(&conn, &ctx.cfg, &outcome);
        apply_suggested_tags(
            &conn,
            article_id,
            &interest,
            &ctx.interest_names,
            ctx.interest_max,
            TAG_KIND_INTEREST,
        )?;
        apply_ai_tags(&conn, article_id, &ai_tags, ctx.ai_max)?;
        return Ok(());
    }

    if run_interest {
        let (system, user) = prompt_interest(&ctx.title, &ctx.summary, &ctx.interest_names);
        let (suggested, outcome) = tags_from_model(
            client,
            &ctx.cfg,
            &system,
            &user,
            parse_interest_payload_outcome,
            Vec::new(),
        )
        .await?;
        let conn = db.lock().await;
        record_outcome(&conn, &ctx.cfg, &outcome);
        apply_suggested_tags(
            &conn,
            article_id,
            &suggested,
            &ctx.interest_names,
            ctx.interest_max,
            TAG_KIND_INTEREST,
        )?;
        return Ok(());
    }

    // AI-only path.
    let (system, user) = prompt_ai(&ctx.title, &ctx.summary, &ctx.ai_names);
    let (suggested, outcome) = tags_from_model(
        client,
        &ctx.cfg,
        &system,
        &user,
        parse_ai_payload_outcome,
        Vec::new(),
    )
    .await?;
    let conn = db.lock().await;
    record_outcome(&conn, &ctx.cfg, &outcome);
    apply_ai_tags(&conn, article_id, &suggested, ctx.ai_max)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, NewArticle};
    use crate::models::SourceType;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db() -> (Connection, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Include thread id so parallel tests never share a path / WAL sidecar.
        let tid = std::thread::current().id();
        let path = std::env::temp_dir().join(format!("papr-auto-tag-{tid:?}-{nanos}.db"));
        let conn = db::open(&path).unwrap();
        (conn, path)
    }

    fn remove_temp_db(conn: Connection, path: PathBuf) {
        drop(conn);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn normalize_trims_and_caps() {
        assert_eq!(normalize_tag_name("  Rust  ").as_deref(), Some("Rust"));
        assert_eq!(normalize_tag_name("").as_deref(), None);
        assert_eq!(normalize_tag_name("   ").as_deref(), None);
        assert_eq!(normalize_tag_name("---").as_deref(), None);
        assert_eq!(normalize_tag_name("!!!").as_deref(), None);
        let long = "a".repeat(40);
        let n = normalize_tag_name(&long).unwrap();
        assert_eq!(n.chars().count(), MAX_TAG_NAME_CHARS);
        assert_eq!(normalize_tag_name("供应链").as_deref(), Some("供应链"));
    }

    #[test]
    fn parse_json_with_fence_and_new() {
        let text =
            "```json\n{\"tags\":[\"Rust\",\"  Go \"], \"new\":[\"WebAssembly\", \"---\"]}\n```";
        let tags = parse_tag_payload(text).unwrap();
        assert_eq!(tags, vec!["Rust", "Go", "WebAssembly"]);
    }

    #[test]
    fn parse_combined_splits_taxonomies() {
        let text = r#"{"interest":["Rust"],"tags":["supply-chain","Go"]}"#;
        let (interest, ai) = parse_combined_payload(text).unwrap();
        assert_eq!(interest, vec!["Rust"]);
        assert_eq!(ai, vec!["supply-chain", "Go"]);
    }

    #[test]
    fn parse_empty_and_prose_are_soft_empty() {
        assert_eq!(
            parse_interest_payload_outcome(""),
            JsonParseOutcome::SoftEmpty
        );
        assert_eq!(
            parse_interest_payload_outcome("   "),
            JsonParseOutcome::SoftEmpty
        );
        assert_eq!(
            parse_interest_payload_outcome("Sorry, I cannot help with that."),
            JsonParseOutcome::SoftEmpty
        );
        // SoftEmpty maps to empty vec via the public parser (no hard error).
        assert!(parse_interest_payload("no braces here").unwrap().is_empty());
    }

    #[test]
    fn parse_bare_array_fallback() {
        assert_eq!(
            parse_interest_payload_outcome(r#"["Rust", "Go"]"#),
            JsonParseOutcome::Ok(vec!["Rust".into(), "Go".into()])
        );
    }

    #[test]
    fn parse_invalid_object_is_invalid() {
        assert!(matches!(
            parse_interest_payload_outcome(r#"{"tags": not-json}"#),
            JsonParseOutcome::Invalid(_)
        ));
        assert!(matches!(
            parse_interest_payload_outcome("{incomplete"),
            JsonParseOutcome::Invalid(_)
        ));
    }

    #[test]
    fn extract_json_object_finds_braces() {
        assert_eq!(
            extract_json_object("here {\"tags\":[]} trailing"),
            Some("{\"tags\":[]}")
        );
        assert_eq!(extract_json_object(""), None);
        assert_eq!(extract_json_object("no object here"), None);
    }

    #[test]
    fn apply_match_only_existing_interest() {
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let article = NewArticle {
            guid: "g1".into(),
            url: Some("https://example.com/a".into()),
            title: "Hello".into(),
            author: None,
            summary: Some("World".into()),
            content_html: None,
            body_text: "World".into(),
            image_url: None,
            published_at: None,
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &article, false, &[]).unwrap());
        let article_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'g1'", [], |r| r.get(0))
            .unwrap();

        db::create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        db::create_tag(&conn, "Go", TAG_KIND_INTEREST).unwrap();
        let existing = vec!["Rust".into(), "Go".into()];
        let suggested = vec![
            "rust".into(), // case-insensitive match
            "NewOne".into(),
            "Go".into(),
            "NewTwo".into(),
        ];
        // Closed vocab: only Rust + Go; unknowns dropped. max_total=1 → Rust only.
        apply_suggested_tags(
            &conn,
            article_id,
            &suggested,
            &existing,
            1,
            TAG_KIND_INTEREST,
        )
        .unwrap();
        let attached = db::tags_for_article(&conn, article_id).unwrap();
        assert_eq!(attached.len(), 1);
        assert_eq!(attached[0].name, "Rust");
        assert_eq!(attached[0].kind, TAG_KIND_INTEREST);

        apply_suggested_tags(
            &conn,
            article_id,
            &suggested,
            &existing,
            5,
            TAG_KIND_INTEREST,
        )
        .unwrap();
        let attached = db::tags_for_article(&conn, article_id).unwrap();
        let names: Vec<_> = attached.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"Rust"));
        assert!(names.contains(&"Go"));
        assert!(!names.contains(&"NewOne"));
        assert_eq!(
            db::list_tags(&conn, Some(TAG_KIND_INTEREST))
                .unwrap()
                .len(),
            2
        );
        assert!(db::list_tags(&conn, Some(TAG_KIND_AI))
            .unwrap()
            .is_empty());

        remove_temp_db(conn, path);
    }

    #[test]
    fn apply_ai_tags_creates_ai_kind() {
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-ai.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let article = NewArticle {
            guid: "gai".into(),
            url: Some("https://example.com/ai".into()),
            title: "AI".into(),
            author: None,
            summary: Some("Tags".into()),
            content_html: None,
            body_text: "Tags".into(),
            image_url: None,
            published_at: None,
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &article, false, &[]).unwrap());
        let article_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'gai'", [], |r| r.get(0))
            .unwrap();

        // Pre-existing interest tag with the same name must not be reused.
        db::create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        apply_ai_tags(
            &conn,
            article_id,
            &["Rust".into(), "WebAssembly".into()],
            5,
        )
        .unwrap();
        let attached = db::tags_for_article(&conn, article_id).unwrap();
        assert_eq!(attached.len(), 2);
        assert!(attached.iter().all(|t| t.kind == TAG_KIND_AI));
        assert_eq!(
            db::list_tags(&conn, Some(TAG_KIND_AI)).unwrap().len(),
            2
        );
        assert_eq!(
            db::list_tags(&conn, Some(TAG_KIND_INTEREST))
                .unwrap()
                .len(),
            1
        );

        remove_temp_db(conn, path);
    }

    #[test]
    fn enqueue_helpers_roundtrip() {
        let (conn, path) = temp_db();
        db::set_setting(&conn, "auto_tag_enabled", "1").unwrap();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed2.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let article = NewArticle {
            guid: "g2".into(),
            url: Some("https://example.com/b".into()),
            title: "Queued".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: None,
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &article, false, &[]).unwrap());
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.pending, 1);

        let job = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.pending, 0);
        assert_eq!(status.processing, 1);

        db::mark_auto_tag_done(&conn, job.0).unwrap();
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.processing, 0);
        assert_eq!(status.done, 1);

        // Recent-done skip (non-force).
        assert!(!db::enqueue_auto_tag(&conn, job.0).unwrap());
        // Force re-enqueue after done.
        assert!(db::enqueue_auto_tag_force(&conn, job.0).unwrap());
        db::mark_auto_tag_failure(&conn, job.0, "boom", 3).unwrap();
        db::mark_auto_tag_failure(&conn, job.0, "boom", 3).unwrap();
        db::mark_auto_tag_failure(&conn, job.0, "final", 3).unwrap();
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.failed, 1);
        assert_eq!(status.last_error.as_deref(), Some("final"));

        remove_temp_db(conn, path);
    }

    #[test]
    fn claim_prefers_newest_article() {
        // Newest-first: COALESCE(published_at, fetched_at) DESC so a deep old
        // backlog cannot starve freshly published inserts.
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-order.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();

        let older = NewArticle {
            guid: "old".into(),
            url: Some("https://example.com/old".into()),
            title: "Old".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2020-01-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        let newer = NewArticle {
            guid: "new".into(),
            url: Some("https://example.com/new".into()),
            title: "New".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &older, false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &newer, false, &[]).unwrap());
        let old_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'old'", [], |r| r.get(0))
            .unwrap();
        let new_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'new'", [], |r| r.get(0))
            .unwrap();
        // Ingest gated off — enqueue explicitly so both are pending.
        db::enqueue_auto_tag(&conn, old_id).unwrap();
        db::enqueue_auto_tag(&conn, new_id).unwrap();

        let first = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(first.0, new_id, "newest article must be claimed first");
        let second = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(second.0, old_id);
        // Claimed rows are processing — no double claim.
        assert!(db::claim_auto_tag_job(&conn).unwrap().is_none());

        remove_temp_db(conn, path);
    }

    #[test]
    fn ingest_skips_enqueue_when_tagging_disabled() {
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-off.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let article = NewArticle {
            guid: "off".into(),
            url: Some("https://example.com/off".into()),
            title: "Off".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: None,
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &article, false, &[]).unwrap());
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.pending, 0);
        assert_eq!(status.done, 0);

        remove_temp_db(conn, path);
    }
}
