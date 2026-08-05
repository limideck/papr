//! Auto-tag articles from title + summary via the configured LLM.
//!
//! Choice B: prefer existing tag names, but allow the model to propose new ones
//! (capped by settings). Tag names are normalised before create/attach.

use crate::ai::{self, AiConfig, MAX_TOKENS};
use crate::db;
use crate::error::{AppError, AppResult};
use rusqlite::Connection;
use serde::Deserialize;

/// Max characters for a stored tag name (after trim).
pub const MAX_TAG_NAME_CHARS: usize = 32;

/// Default: at most this many brand-new tags per article.
pub const DEFAULT_MAX_NEW_PER_ARTICLE: i64 = 3;

/// Default: at most this many tags attached to one article (existing + new).
pub const DEFAULT_MAX_TAGS_PER_ARTICLE: i64 = 5;

/// Failed jobs stop retrying after this many attempts.
pub const MAX_ATTEMPTS: i64 = 3;

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
struct AiTagPayload {
    #[serde(default)]
    tags: Vec<String>,
    /// Optional explicit "new" list; merged into `tags` for matching.
    #[serde(default)]
    new: Vec<String>,
}

/// Pull a JSON object out of model output (tolerates markdown fences / prose).
pub fn parse_tag_payload(text: &str) -> AppResult<Vec<String>> {
    let json_str = extract_json_object(text).ok_or_else(|| {
        AppError::other(format!(
            "auto-tag: no JSON object in model response: {}",
            text.chars().take(120).collect::<String>()
        ))
    })?;
    let payload: AiTagPayload = serde_json::from_str(json_str)
        .map_err(|e| AppError::other(format!("auto-tag: invalid JSON: {e}")))?;
    let mut out = Vec::new();
    for name in payload.tags.into_iter().chain(payload.new) {
        if let Some(n) = normalize_tag_name(&name) {
            if !out.iter().any(|e: &String| e.eq_ignore_ascii_case(&n)) {
                out.push(n);
            }
        }
    }
    Ok(out)
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&text[start..=end])
}

pub fn prompt_for(title: &str, summary: &str, existing: &[String]) -> (String, String) {
    let system = "You tag news articles for an RSS reader. \
Reply with ONLY a JSON object, no markdown, no commentary. \
Format: {\"tags\":[\"name1\",\"name2\"]}. \
Prefer names from the existing-tags list (exact spelling). \
You may propose short new tags when nothing fits. \
Use at most 5 tags. Tag names must be short (under 32 characters). \
An empty tags array is fine when nothing is relevant.";
    let existing_list = if existing.is_empty() {
        "(none yet)".to_string()
    } else {
        existing.join(", ")
    };
    let summary = {
        let s = summary.trim();
        if s.is_empty() {
            "(no summary)".to_string()
        } else {
            s.chars().take(800).collect()
        }
    };
    let user = format!(
        "Existing tags: {existing_list}\n\nTitle: {title}\n\nSummary: {summary}"
    );
    (system.to_string(), user)
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

/// Match / create / attach tags respecting new-tag and total caps.
pub fn apply_suggested_tags(
    conn: &Connection,
    article_id: i64,
    suggested: &[String],
    existing_names: &[String],
    max_new: i64,
    max_total: i64,
) -> AppResult<()> {
    let already = db::article_tag_count(conn, article_id)?;
    let mut slots = (max_total - already).max(0);
    let mut new_budget = max_new;
    let mut known: Vec<String> = existing_names.to_vec();

    for name in suggested {
        if slots <= 0 {
            break;
        }
        let Some(name) = normalize_tag_name(name) else {
            continue;
        };
        let is_existing = known.iter().any(|e| e.eq_ignore_ascii_case(&name));
        if !is_existing {
            if new_budget <= 0 {
                continue;
            }
            new_budget -= 1;
            known.push(name.clone());
        }

        let tag_id = db::create_tag(conn, &name)?;
        db::set_article_tag(conn, article_id, tag_id, true)?;
        slots -= 1;
    }
    Ok(())
}

/// Run one auto-tag job end-to-end (caller holds no DB lock across the AI call).
///
/// Steps: load context → LLM → apply tags. Returns `Ok(())` on success.
pub async fn process_article(
    db: &tokio::sync::Mutex<Connection>,
    client: &reqwest::Client,
    article_id: i64,
) -> AppResult<()> {
    let (title, summary, existing_names, max_new, max_total, cfg) = {
        let conn = db.lock().await;
        if !db::setting_flag(&conn, "auto_tag_enabled", false) {
            return Err(AppError::code("autoTagDisabled"));
        }
        let max_new =
            db::setting_parsed(&conn, "auto_tag_max_new_per_article", DEFAULT_MAX_NEW_PER_ARTICLE)
                .max(0);
        let max_total =
            db::setting_parsed(&conn, "auto_tag_max_tags_per_article", DEFAULT_MAX_TAGS_PER_ARTICLE)
                .max(0);
        let (title, summary) = load_article_text(&conn, article_id)?;
        let existing_names: Vec<String> = db::list_tags(&conn)?
            .into_iter()
            .map(|t| t.name)
            .collect();
        let cfg = AiConfig::new(
            db::get_setting(&conn, "ai_provider")?,
            db::get_setting(&conn, "ai_api_key")?,
            db::get_setting(&conn, "ai_model")?,
            db::get_setting(&conn, "ai_base_url")?,
        )?;
        (title, summary, existing_names, max_new, max_total, cfg)
    };

    let (system, user) = prompt_for(&title, &summary, &existing_names);
    let response = ai::complete_chat(client, &cfg, &system, &user, MAX_TOKENS).await?;
    let suggested = parse_tag_payload(&response)?;

    let conn = db.lock().await;
    apply_suggested_tags(
        &conn,
        article_id,
        &suggested,
        &existing_names,
        max_new,
        max_total,
    )?;
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
        let path = std::env::temp_dir().join(format!("papr-auto-tag-{nanos}.db"));
        let conn = db::open(&path).unwrap();
        (conn, path)
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
    fn apply_respects_caps() {
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

        db::create_tag(&conn, "Rust").unwrap();
        let existing = vec!["Rust".into()];
        let suggested = vec![
            "Rust".into(),
            "NewOne".into(),
            "NewTwo".into(),
            "NewThree".into(),
            "NewFour".into(),
        ];
        // max_new=2, max_total=3 → Rust + NewOne + NewTwo
        apply_suggested_tags(&conn, article_id, &suggested, &existing, 2, 3).unwrap();
        let attached = db::tags_for_article(&conn, article_id).unwrap();
        assert_eq!(attached.len(), 3);
        let names: Vec<_> = attached.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Rust"));
        assert!(names.contains(&"NewOne"));
        assert!(names.contains(&"NewTwo"));
        assert!(!names.contains(&"NewThree"));

        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn enqueue_helpers_roundtrip() {
        let (conn, path) = temp_db();
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
        db::mark_auto_tag_done(&conn, job.0).unwrap();
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.pending, 0);

        // Failure path with retries.
        db::enqueue_auto_tag(&conn, job.0).unwrap();
        db::mark_auto_tag_failure(&conn, job.0, "boom", 3).unwrap();
        db::mark_auto_tag_failure(&conn, job.0, "boom", 3).unwrap();
        db::mark_auto_tag_failure(&conn, job.0, "final", 3).unwrap();
        let status = db::auto_tag_queue_status(&conn).unwrap();
        assert_eq!(status.failed, 1);
        assert_eq!(status.last_error.as_deref(), Some("final"));

        drop(conn);
        let _ = std::fs::remove_file(path);
    }
}
