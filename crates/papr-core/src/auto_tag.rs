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
use serde_json::Value;

/// Max characters for a stored tag name (after trim).
pub const MAX_TAG_NAME_CHARS: usize = 32;

/// Default: at most this many tags of one kind attached to one article.
pub const DEFAULT_MAX_TAGS_PER_ARTICLE: i64 = 5;

/// Failed jobs stop retrying after this many attempts.
pub const MAX_ATTEMPTS: i64 = 3;

/// Output token cap for tagging. JSON is tiny, but reasoning models may spend
/// budget before the final object — leave a little headroom.
pub const TAG_MAX_TOKENS: u32 = 256;

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

/// How model text maps to tags before apply.
#[derive(Debug, PartialEq, Eq)]
pub enum JsonParseOutcome<T> {
    /// Parsed successfully (may be an empty tag list).
    Ok(T),
    /// Empty / prose / unusable — soft-skip (do not burn retries as hard failures).
    SoftEmpty,
    /// Looks like truncated/broken JSON — candidate for one repair call.
    Invalid(String),
}

/// Strip markdown fences and common reasoning wrappers before JSON scan.
pub fn preprocess_model_text(text: &str) -> String {
    let mut s = text.trim().to_string();
    for (open, close) in [
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
        ("<reasoning>", "</reasoning>"),
        ("<redacted_reasoning>", "</redacted_reasoning>"),
    ] {
        while let Some(start) = s.to_ascii_lowercase().find(open) {
            let after_open = start + open.len();
            let rest_lower = s[after_open..].to_ascii_lowercase();
            if let Some(rel) = rest_lower.find(close) {
                let end = after_open + rel + close.len();
                s.replace_range(start..end, " ");
            } else {
                // Unclosed reasoning block — drop from open tag to end.
                s.truncate(start);
                break;
            }
        }
    }
    // ```json ... ``` / ``` ... ```
    if let Some(start) = s.find("```") {
        let after = start + 3;
        let body_start = s[after..]
            .find('\n')
            .map(|i| after + i + 1)
            .unwrap_or(after);
        if let Some(rel_end) = s[body_start..].find("```") {
            let body = s[body_start..body_start + rel_end].trim();
            return body.to_string();
        }
        // Opening fence without close — take everything after the language line.
        return s[body_start..].trim().to_string();
    }
    s.trim().to_string()
}

/// Drop trailing commas before `]` / `}` (common LLM slip). Respects strings.
pub fn sanitize_json_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == ']' || chars[j] == '}') {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Balanced `{...}` slices (string-aware), each trailing-comma sanitized.
/// Public for unit tests.
pub fn extract_json_objects(text: &str) -> Vec<String> {
    extract_balanced(text, '{', '}')
        .into_iter()
        .map(|s| sanitize_json_trailing_commas(&s))
        .collect()
}

/// Balanced `[...]` slices (string-aware), each trailing-comma sanitized.
fn extract_json_arrays(text: &str) -> Vec<String> {
    extract_balanced(text, '[', ']')
        .into_iter()
        .map(|s| sanitize_json_trailing_commas(&s))
        .collect()
}

fn extract_balanced(text: &str, open: char, close: char) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != open {
            i += 1;
            continue;
        }
        let start = i;
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escape = false;
        let mut ended = false;
        while i < chars.len() {
            let c = chars[i];
            if in_string {
                if escape {
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            match c {
                '"' => in_string = true,
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        let slice: String = chars[start..=i].iter().collect();
                        out.push(slice);
                        i += 1;
                        ended = true;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if !ended {
            // Truncated from this opener — stop; remainder is not a complete value.
            break;
        }
    }
    out
}

/// Best-effort single object: last balanced `{...}` after preprocess (answer
/// after reasoning). Public for unit tests / older call sites.
pub fn extract_json_object(text: &str) -> Option<String> {
    let cleaned = preprocess_model_text(text);
    extract_json_objects(&cleaned).into_iter().last()
}

fn strings_from_json_array(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect()
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

fn names_from_object_keys(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let mut names = Vec::new();
    for key in keys {
        names.extend(strings_from_json_array(obj.get(*key)));
    }
    dedupe_names(names)
}

/// True when the text looks like an unfinished `{...}` / `[...]` (repair-worthy).
fn looks_truncated_json(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let has_obj_open = t.contains('{');
    let has_arr_open = t.contains('[');
    if !has_obj_open && !has_arr_open {
        return false;
    }
    let objs = extract_json_objects(t);
    let arrs = extract_json_arrays(t);
    // Complete values exist → not truncated (may still be wrong shape).
    if !objs.is_empty() || !arrs.is_empty() {
        return false;
    }
    true
}

/// Balanced `{...}` that still looks like JSON (quotes/colon) but failed to parse.
fn looks_like_json_object(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('{') && (t.contains('"') || t.contains(':'))
}

/// Bare `["a","b"]` when the model skipped the object wrapper.
fn parse_string_array_fallback(text: &str) -> Option<Vec<String>> {
    for arr in extract_json_arrays(text).into_iter().rev() {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&arr) {
            let names: Vec<String> = items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            // Only accept arrays of strings (tag lists), not nested junk.
            if items.iter().all(|v| v.is_string() || v.is_null()) {
                return Some(dedupe_names(names));
            }
        }
    }
    None
}

fn parse_tag_list_outcome(text: &str, object_keys: &[&str]) -> JsonParseOutcome<Vec<String>> {
    let cleaned = preprocess_model_text(text);
    if cleaned.is_empty() {
        return JsonParseOutcome::SoftEmpty;
    }

    // Prefer the last object (final answer after chain-of-thought braces).
    let mut malformed_json = None;
    for obj in extract_json_objects(&cleaned).into_iter().rev() {
        match serde_json::from_str::<Value>(&obj) {
            Ok(Value::Object(map)) => {
                // Valid object → success even when tag arrays are empty / null / absent.
                return JsonParseOutcome::Ok(names_from_object_keys(&map, object_keys));
            }
            Ok(_) => continue,
            Err(e) if looks_like_json_object(&obj) => {
                malformed_json = Some(e.to_string());
            }
            Err(_) => continue,
        }
    }
    if let Some(err) = malformed_json {
        return JsonParseOutcome::Invalid(err);
    }

    if let Some(names) = parse_string_array_fallback(&cleaned) {
        return JsonParseOutcome::Ok(names);
    }

    if looks_truncated_json(&cleaned) {
        return JsonParseOutcome::Invalid("truncated or incomplete JSON in model response".into());
    }

    // Prose, reasoning-only, "no tags", or balanced non-JSON braces → empty done.
    JsonParseOutcome::SoftEmpty
}

/// Parse closed-vocabulary interest suggestions from model output.
pub fn parse_interest_payload_outcome(text: &str) -> JsonParseOutcome<Vec<String>> {
    parse_tag_list_outcome(text, &["tags", "new", "interest"])
}

/// Parse free-form AI tag suggestions from model output.
pub fn parse_ai_payload_outcome(text: &str) -> JsonParseOutcome<Vec<String>> {
    parse_tag_list_outcome(text, &["tags", "ai", "new"])
}

/// Parse a combined interest + AI response.
pub fn parse_combined_payload_outcome(
    text: &str,
) -> JsonParseOutcome<(Vec<String>, Vec<String>)> {
    let cleaned = preprocess_model_text(text);
    if cleaned.is_empty() {
        return JsonParseOutcome::SoftEmpty;
    }

    let mut malformed_json = None;
    for obj in extract_json_objects(&cleaned).into_iter().rev() {
        match serde_json::from_str::<Value>(&obj) {
            Ok(Value::Object(map)) => {
                let interest = names_from_object_keys(&map, &["interest"]);
                let ai = names_from_object_keys(&map, &["tags", "ai", "new"]);
                return JsonParseOutcome::Ok((interest, ai));
            }
            Ok(_) => continue,
            Err(e) if looks_like_json_object(&obj) => {
                malformed_json = Some(e.to_string());
            }
            Err(_) => continue,
        }
    }
    if let Some(err) = malformed_json {
        return JsonParseOutcome::Invalid(err);
    }

    // Bare string array: treat as AI free-form only (interest stays closed).
    if let Some(names) = parse_string_array_fallback(&cleaned) {
        return JsonParseOutcome::Ok((Vec::new(), names));
    }

    if looks_truncated_json(&cleaned) {
        return JsonParseOutcome::Invalid("truncated or incomplete JSON in model response".into());
    }

    JsonParseOutcome::SoftEmpty
}

/// Parse closed-vocabulary interest suggestions from model output.
/// Empty / non-JSON responses yield an empty list (soft success).
pub fn parse_interest_payload(text: &str) -> AppResult<Vec<String>> {
    match parse_interest_payload_outcome(text) {
        JsonParseOutcome::Ok(v) => Ok(v),
        JsonParseOutcome::SoftEmpty => Ok(Vec::new()),
        // Public API: Invalid still errors; the worker soft-completes via outcome + repair.
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
            // One cheap repair; if the repair call itself fails, soft-complete
            // rather than burning the job into the permanent failed pile.
            match repair_tag_json(client, cfg, system, &outcome.text).await {
                Ok(repaired) => {
                    outcome.usage += repaired.usage;
                    let text = repaired.text;
                    match parse(&text) {
                        JsonParseOutcome::Ok(v) => {
                            outcome.text = text;
                            Ok((v, outcome))
                        }
                        JsonParseOutcome::SoftEmpty | JsonParseOutcome::Invalid(_) => {
                            outcome.text = text;
                            Ok((soft_empty, outcome))
                        }
                    }
                }
                Err(_) => Ok((soft_empty, outcome)),
            }
        }
    }
}

/// Run one auto-tag job end-to-end (caller holds no DB lock across the AI call).
///
/// Steps: load context → skip if at caps → LLM (+ one JSON repair) → apply tags.
/// At-cap skip still applies because apply_* cannot attach more tags past the
/// configured max (force backfill may re-queue `done`, but capped articles
/// skip the LLM).
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
        assert_eq!(
            parse_interest_payload_outcome("No relevant tags."),
            JsonParseOutcome::SoftEmpty
        );
        assert_eq!(
            parse_interest_payload_outcome("none"),
            JsonParseOutcome::SoftEmpty
        );
        // SoftEmpty maps to empty vec via the public parser (no hard error).
        assert!(parse_interest_payload("no braces here").unwrap().is_empty());
        // Valid object with empty / null tags → Ok(empty), not a failure.
        assert_eq!(
            parse_interest_payload_outcome(r#"{"tags":[]}"#),
            JsonParseOutcome::Ok(vec![])
        );
        assert_eq!(
            parse_interest_payload_outcome(r#"{"tags":null}"#),
            JsonParseOutcome::Ok(vec![])
        );
    }

    #[test]
    fn parse_bare_array_fallback() {
        assert_eq!(
            parse_interest_payload_outcome(r#"["Rust", "Go"]"#),
            JsonParseOutcome::Ok(vec!["Rust".into(), "Go".into()])
        );
        assert_eq!(
            parse_interest_payload_outcome("```\n[\"Rust\"]\n```"),
            JsonParseOutcome::Ok(vec!["Rust".into()])
        );
    }

    #[test]
    fn parse_trailing_commas_and_fences() {
        let text = "```json\n{\"tags\":[\"Rust\",],}\n```";
        assert_eq!(
            parse_interest_payload_outcome(text),
            JsonParseOutcome::Ok(vec!["Rust".into()])
        );
    }

    #[test]
    fn parse_prefers_last_object_after_reasoning() {
        let text = r#"Thinking {about this} briefly.
<think>internal {noise}</think>
{"tags":["Go"]}
Final: {"tags":["Rust","Go"]}"#;
        assert_eq!(
            parse_interest_payload_outcome(text),
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
    fn parse_prose_braces_are_soft_empty() {
        // Balanced braces that are not JSON-like → soft empty, not repair.
        assert_eq!(
            parse_interest_payload_outcome("I looked at {the article} and found nothing."),
            JsonParseOutcome::SoftEmpty
        );
    }

    #[test]
    fn extract_json_object_finds_braces() {
        assert_eq!(
            extract_json_object("here {\"tags\":[]} trailing"),
            Some("{\"tags\":[]}".into())
        );
        assert_eq!(extract_json_object(""), None);
        assert_eq!(extract_json_object("no object here"), None);
        // Nested braces in strings stay intact.
        assert_eq!(
            extract_json_object(r#"{"tags":["a{b}"]}"#),
            Some(r#"{"tags":["a{b}"]}"#.into())
        );
    }

    #[test]
    fn sanitize_strips_trailing_commas() {
        assert_eq!(
            sanitize_json_trailing_commas(r#"{"tags":["a",],}"#),
            r#"{"tags":["a"]}"#
        );
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
    fn clear_queue_drops_active_keeps_done() {
        let (conn, path) = temp_db();
        db::set_setting(&conn, "auto_tag_enabled", "1").unwrap();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-clear.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let mk = |guid: &str, title: &str| NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: None,
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &mk("c1", "Pending"), false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &mk("c2", "Processing"), false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &mk("c3", "Failed"), false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &mk("c4", "Done"), false, &[]).unwrap());

        let pending_id = db::claim_auto_tag_job(&conn).unwrap().unwrap().0; // newest first
        // Leave one processing; release another path: mark one failed, one done.
        let ids: Vec<i64> = conn
            .prepare("SELECT article_id FROM auto_tag_queue ORDER BY article_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(ids.len(), 4);
        // One is already processing (claimed). Pick others for failed/done/pending.
        let mut rest: Vec<i64> = ids.into_iter().filter(|&id| id != pending_id).collect();
        let done_id = rest.pop().unwrap();
        let failed_id = rest.pop().unwrap();
        // `rest` still has one pending.
        db::mark_auto_tag_done(&conn, done_id).unwrap();
        db::mark_auto_tag_failure(&conn, failed_id, "x", 1).unwrap();

        let before = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(before.pending, 1);
        assert_eq!(before.processing, 1);
        assert_eq!(before.failed, 1);
        assert_eq!(before.done, 1);

        let cleared = db::clear_auto_tag_queue(&conn).unwrap();
        assert_eq!(cleared, 3);
        let after = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(after.pending, 0);
        assert_eq!(after.processing, 0);
        assert_eq!(after.failed, 0);
        assert_eq!(after.done, 1);
        assert_eq!(db::clear_auto_tag_queue(&conn).unwrap(), 0);

        remove_temp_db(conn, path);
    }

    #[test]
    fn backfill_requeues_done_untagged_skips_tagged() {
        let (conn, path) = temp_db();
        db::set_setting(&conn, "auto_tag_enabled", "1").unwrap();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-bf.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let empty = NewArticle {
            guid: "bf-empty".into(),
            url: Some("https://example.com/bf-empty".into()),
            title: "Empty done".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some(now.clone()),
            enclosures: vec![],
        };
        let tagged = NewArticle {
            guid: "bf-tagged".into(),
            url: Some("https://example.com/bf-tagged".into()),
            title: "Tagged done".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some(now),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &empty, false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &tagged, false, &[]).unwrap());
        let empty_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'bf-empty'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let tagged_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'bf-tagged'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Both start pending from ingest; mark done. Attach a tag only to one.
        for _ in 0..2 {
            let claimed = db::claim_auto_tag_job(&conn).unwrap().unwrap().0;
            assert!(claimed == empty_id || claimed == tagged_id);
            db::mark_auto_tag_done(&conn, claimed).unwrap();
        }
        let tag_id = db::create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        db::set_article_tag(&conn, tagged_id, tag_id, true).unwrap();

        // Default: re-queue done-with-zero-tags; leave done-with-tags alone.
        assert_eq!(db::enqueue_auto_tag_backfill(&conn, 7, false).unwrap(), 1);
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 1);
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().done, 1);
        let pending_id: i64 = conn
            .query_row(
                "SELECT article_id FROM auto_tag_queue WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending_id, empty_id);

        // Force also re-queues tagged done.
        assert_eq!(db::enqueue_auto_tag_backfill(&conn, 7, true).unwrap(), 1);
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 2);

        // Failed is re-queued even without force (and even if it somehow has tags).
        let id = db::claim_auto_tag_job(&conn).unwrap().unwrap().0;
        db::mark_auto_tag_failure(&conn, id, "x", 1).unwrap();
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().failed, 1);
        assert_eq!(db::enqueue_auto_tag_backfill(&conn, 7, false).unwrap(), 1);

        let window = db::auto_tag_window_stats(&conn, 7).unwrap();
        assert_eq!(window.articles, 2);
        assert_eq!(window.untagged, 1);
        assert_eq!(window.tagged, 1);

        remove_temp_db(conn, path);
    }

    #[test]
    fn claim_prefers_newer_fetched_over_newer_published() {
        // Primary sort is fetched_at DESC: an older-fetched article with a
        // newer published_at must NOT jump ahead of a just-ingested item.
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

        let older_fetch = NewArticle {
            guid: "old-fetch".into(),
            url: Some("https://example.com/old-fetch".into()),
            title: "Old fetch, new publish".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-06T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        let newer_fetch = NewArticle {
            guid: "new-fetch".into(),
            url: Some("https://example.com/new-fetch".into()),
            title: "New fetch, old publish".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2020-01-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &older_fetch, false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &newer_fetch, false, &[]).unwrap());
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'old-fetch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'new-fetch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Pin fetched_at so the newer-publish article is clearly older-fetched.
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-01 10:00:00' WHERE id = ?1",
            rusqlite::params![old_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-07 12:00:00' WHERE id = ?1",
            rusqlite::params![new_id],
        )
        .unwrap();
        db::enqueue_auto_tag(&conn, old_id).unwrap();
        db::enqueue_auto_tag(&conn, new_id).unwrap();

        let first = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(
            first.0, new_id,
            "newer fetched_at must be claimed first even with older published_at"
        );
        let second = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(second.0, old_id);
        assert!(db::claim_auto_tag_job(&conn).unwrap().is_none());

        remove_temp_db(conn, path);
    }

    #[test]
    fn claim_new_ingest_cuts_ahead_of_pending_backlog() {
        // While A is still pending, B ingested later must jump the queue.
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-cutin.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();

        let a = NewArticle {
            guid: "a-backlog".into(),
            url: Some("https://example.com/a-backlog".into()),
            title: "Backlog A".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-06T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &a, false, &[]).unwrap());
        let a_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'a-backlog'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-01 08:00:00' WHERE id = ?1",
            rusqlite::params![a_id],
        )
        .unwrap();
        db::enqueue_auto_tag(&conn, a_id).unwrap();
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 1);

        let b = NewArticle {
            guid: "b-fresh".into(),
            url: Some("https://example.com/b-fresh".into()),
            title: "Fresh B".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            // Ancient publish date must not keep B behind A.
            published_at: Some("2019-01-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &b, false, &[]).unwrap());
        let b_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'b-fresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-07 15:00:00' WHERE id = ?1",
            rusqlite::params![b_id],
        )
        .unwrap();
        db::enqueue_auto_tag(&conn, b_id).unwrap();
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 2);

        let first = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(
            first.0, b_id,
            "later-fetched B must cut ahead of still-pending earlier-fetched A"
        );
        let second = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(second.0, a_id);

        remove_temp_db(conn, path);
    }

    #[test]
    fn backfill_days_zero_includes_ancient_untagged() {
        // days=0 = whole library; ancient published_at outside any N-day window.
        let (conn, path) = temp_db();
        db::set_setting(&conn, "auto_tag_enabled", "1").unwrap();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-ancient.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let ancient = NewArticle {
            guid: "ancient".into(),
            url: Some("https://example.com/ancient".into()),
            title: "Ancient".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2015-01-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &ancient, false, &[]).unwrap());
        let id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'ancient'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Clear ingest enqueue so backfill is what re-queues it.
        conn.execute("DELETE FROM auto_tag_queue WHERE article_id = ?1", rusqlite::params![id])
            .unwrap();

        assert_eq!(
            db::enqueue_auto_tag_backfill(&conn, 7, false).unwrap(),
            0,
            "7-day window must miss ancient publish date"
        );
        assert_eq!(db::enqueue_auto_tag_backfill(&conn, 0, false).unwrap(), 1);
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 1);

        let window = db::auto_tag_window_stats(&conn, 0).unwrap();
        assert_eq!(window.days, 0);
        assert_eq!(window.articles, 1);
        assert_eq!(window.untagged, 1);

        remove_temp_db(conn, path);
    }

    #[test]
    fn backfill_does_not_boost_old_over_newer_pending() {
        // Backfill bumps queue.updated_at, but claim orders by fetched_at —
        // re-queuing an older failed item must not jump ahead of a newer pending.
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-bf-prio.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();

        let older = NewArticle {
            guid: "old-fail".into(),
            url: Some("https://example.com/old-fail".into()),
            title: "Old fail".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        let newer = NewArticle {
            guid: "new-pend".into(),
            url: Some("https://example.com/new-pend".into()),
            title: "New pending".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-06T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &older, false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &newer, false, &[]).unwrap());
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'old-fail'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'new-pend'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-01 09:00:00' WHERE id = ?1",
            rusqlite::params![old_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-06 09:00:00' WHERE id = ?1",
            rusqlite::params![new_id],
        )
        .unwrap();

        db::enqueue_auto_tag(&conn, old_id).unwrap();
        let claimed = db::claim_auto_tag_job(&conn).unwrap().unwrap().0;
        assert_eq!(claimed, old_id);
        db::mark_auto_tag_failure(&conn, old_id, "boom", 1).unwrap();

        db::enqueue_auto_tag(&conn, new_id).unwrap();
        assert_eq!(db::enqueue_auto_tag_backfill(&conn, 30, false).unwrap(), 1);
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 2);

        let first = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(
            first.0, new_id,
            "newer pending must stay ahead after backfill re-queues older failed"
        );
        let second = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(second.0, old_id);

        remove_temp_db(conn, path);
    }

    #[test]
    fn backfill_zero_tag_redo_does_not_boost_old_over_newer_pending() {
        // Default 补打 re-queues done-with-zero-tags. Claim still orders by
        // fetched_at — an older 0-tag redo must not jump a newer pending.
        let (conn, path) = temp_db();
        let feed_id = db::insert_feed(
            &conn,
            "https://example.com/feed-bf-zerotag.xml",
            None,
            "Example",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();

        let older = NewArticle {
            guid: "old-zerotag".into(),
            url: Some("https://example.com/old-zerotag".into()),
            title: "Old zero-tag".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-01T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        let newer = NewArticle {
            guid: "new-pend-zt".into(),
            url: Some("https://example.com/new-pend-zt".into()),
            title: "New pending".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "".into(),
            image_url: None,
            published_at: Some("2026-08-06T00:00:00+00:00".into()),
            enclosures: vec![],
        };
        assert!(db::upsert_article(&conn, feed_id, &older, false, &[]).unwrap());
        assert!(db::upsert_article(&conn, feed_id, &newer, false, &[]).unwrap());
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'old-zerotag'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'new-pend-zt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-01 09:00:00' WHERE id = ?1",
            rusqlite::params![old_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2026-08-06 09:00:00' WHERE id = ?1",
            rusqlite::params![new_id],
        )
        .unwrap();

        db::enqueue_auto_tag(&conn, old_id).unwrap();
        let claimed = db::claim_auto_tag_job(&conn).unwrap().unwrap().0;
        assert_eq!(claimed, old_id);
        // Done with zero tags — the 0-tag redo path for default backfill.
        db::mark_auto_tag_done(&conn, old_id).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM article_tags WHERE article_id = ?1",
                rusqlite::params![old_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );

        db::enqueue_auto_tag(&conn, new_id).unwrap();
        assert_eq!(db::enqueue_auto_tag_backfill(&conn, 30, false).unwrap(), 1);
        assert_eq!(db::auto_tag_queue_status(&conn).unwrap().pending, 2);

        let first = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(
            first.0, new_id,
            "newer pending must stay ahead after backfill re-queues older 0-tag done"
        );
        let second = db::claim_auto_tag_job(&conn).unwrap().unwrap();
        assert_eq!(second.0, old_id);

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
