//! Deterministic interest shelves ("interest tag definitions").
//!
//! Interest tags are normally attached only when the LLM happens to pick them
//! from the closed vocabulary — a per-article guess that leaves shelves like
//! `AI、芯片及算力相关` systematically incomplete (the model sees a bare tag
//! name, not the scope the admin had in mind).
//!
//! A tag with a `tags.definition` JSON (interest kind only) turns into a
//! **rule**: an article belongs on that shelf when it already carries an AI
//! tag from one of the linked AI topic subtrees (`aiTopics`), or its
//! title/summary contains any `keywords` — and none of `exclude` matches.
//! This is applied deterministically after auto-tagging (and by the
//! `papr tag backfill-interest` CLI for history), independent of the model's
//! guesses, so the shelf finally reflects the admin's intent.

use crate::db;
use crate::error::AppResult;
use crate::models::TAG_KIND_AI;
use rusqlite::{params, Connection};
use serde::Deserialize;

/// One shelf rule, decoded from `tags.definition` (JSON).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InterestDefinition {
    /// Human description of the shelf's scope (shown to the operator).
    #[serde(default)]
    pub description: Option<String>,
    /// Substring keywords: a hit in title/summary attaches the tag.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Substring exclusions: a hit vetoes the attachment even on a family hit.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// AI-vocabulary topic names whose subtree (topic + direct children)
    /// counts as an anchor: an article tagged inside one is a shelf member.
    #[serde(default)]
    pub ai_topics: Vec<String>,
}

/// An interest tag plus its resolved, ready-to-test rule.
#[derive(Debug, Clone)]
pub struct InterestRule {
    pub tag_id: i64,
    pub tag_name: String,
    pub definition: InterestDefinition,
    /// Anchor tag ids (linked AI topics + their children).
    family_ids: std::collections::HashSet<i64>,
}

fn parse_definition(raw: &str) -> Option<InterestDefinition> {
    match serde_json::from_str::<InterestDefinition>(raw) {
        Ok(d) => Some(d),
        Err(e) => {
            log::warn!("interest definition parse failed: {e}");
            None
        }
    }
}

/// Load every interest tag that carries a usable definition.
fn load_rules(conn: &Connection) -> AppResult<Vec<InterestRule>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, definition FROM tags
         WHERE kind = ?1 AND definition IS NOT NULL AND trim(definition) != ''",
    )?;
    let rows = stmt
        .query_map(params![crate::models::TAG_KIND_INTEREST], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (id, name, raw) in rows {
        let Some(definition) = parse_definition(&raw) else {
            continue;
        };
        let mut family_ids = std::collections::HashSet::new();
        for topic in &definition.ai_topics {
            // Exact name or pinned alias of the AI vocabulary.
            let Some(tid) = db::resolve_tag_by_name_or_alias(conn, TAG_KIND_AI, topic)? else {
                continue;
            };
            family_ids.insert(tid);
            let mut kids = conn.prepare("SELECT id FROM tags WHERE parent_id = ?1")?;
            let child_rows = kids
                .query_map(params![tid], |r| r.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            family_ids.extend(child_rows);
        }
        out.push(InterestRule {
            tag_id: id,
            tag_name: name,
            definition,
            family_ids,
        });
    }
    Ok(out)
}

/// True when any interest tag has a definition (cheap gate for hot paths).
pub fn has_rules(conn: &Connection) -> AppResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags
         WHERE kind = ?1 AND definition IS NOT NULL AND trim(definition) != ''",
        params![crate::models::TAG_KIND_INTEREST],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn lower_contains(text: &str, needles: &[String]) -> bool {
    let hay = text.to_lowercase();
    needles
        .iter()
        .any(|n| !n.is_empty() && hay.contains(&n.to_lowercase()))
}

fn article_text(conn: &Connection, article_id: i64) -> AppResult<String> {
    let (title, summary) = crate::auto_tag::load_article_text(conn, article_id)?;
    Ok(format!("{title} {summary}"))
}

/// The article's AI-vocabulary tag ids (used for the family anchor test).
fn article_ai_tag_ids(conn: &Connection, article_id: i64) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT at.tag_id FROM article_tags at
         JOIN tags t ON t.id = at.tag_id
         WHERE at.article_id = ?1 AND t.kind = ?2",
    )?;
    let rows = stmt
        .query_map(params![article_id, TAG_KIND_AI], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Does this article satisfy the rule? Family anchor OR keyword hit, minus any
/// exclusion match.
fn rule_matches(conn: &Connection, rule: &InterestRule, article_id: i64) -> AppResult<bool> {
    let text = article_text(conn, article_id)?;
    if !rule.definition.exclude.is_empty() && lower_contains(&text, &rule.definition.exclude) {
        return Ok(false);
    }
    if !rule.family_ids.is_empty() {
        let ai_ids = article_ai_tag_ids(conn, article_id)?;
        if ai_ids.iter().any(|id| rule.family_ids.contains(id)) {
            return Ok(true);
        }
    }
    if !rule.definition.keywords.is_empty() {
        return Ok(lower_contains(&text, &rule.definition.keywords));
    }
    Ok(false)
}

/// Deterministic pass for one freshly tagged article: attach every shelf whose
/// rule matches, unless the article is already on that shelf. Returns how many
/// tags were attached. The caller gates on interest auto-matching being
/// enabled.
pub fn attach_for_article(conn: &Connection, article_id: i64) -> AppResult<usize> {
    if !has_rules(conn)? {
        return Ok(0);
    }
    let rules = load_rules(conn)?;
    let already: std::collections::HashSet<i64> = conn
        .prepare(
            "SELECT tag_id FROM article_tags at
             JOIN tags t ON t.id = at.tag_id
             WHERE at.article_id = ?1 AND t.kind = ?2",
        )?
        .query_map(params![article_id, crate::models::TAG_KIND_INTEREST], |r| {
            r.get::<_, i64>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .collect();
    let mut n = 0usize;
    for rule in &rules {
        if already.contains(&rule.tag_id) {
            continue;
        }
        if rule_matches(conn, rule, article_id)? {
            db::set_article_tag(conn, article_id, rule.tag_id, true)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Report for the history backfill pass.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillReport {
    /// Number of interest tags carrying a definition.
    pub rules: usize,
    /// Per-rule: shelf name → articles matched but not already on the shelf.
    pub would_add: Vec<(String, usize)>,
    /// Distinct articles matched by at least one rule and not already shelved.
    pub distinct_articles: usize,
    /// True when `apply` ran (matches were attached).
    pub applied: bool,
}

/// Re-run every shelf rule over all articles (history backfill).
///
/// `apply = false` only counts (dry-run); `apply = true` attaches through the
/// normal article-tag path so index/undo bookkeeping stays intact. Deterministic
/// shelves are recall-first: they may push an article past the per-article
/// interest cap used for LLM suggestions.
pub fn backfill(conn: &Connection, apply: bool) -> AppResult<BackfillReport> {
    let rules = load_rules(conn)?;
    let mut would_add: Vec<(String, usize)> = Vec::new();
    let mut matched_all: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for rule in &rules {
        // Already on this shelf (before any adds below).
        let mut attached: std::collections::HashSet<i64> = conn
            .prepare("SELECT article_id FROM article_tags WHERE tag_id = ?1")?
            .query_map(params![rule.tag_id], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect();

        let mut candidates: std::collections::HashSet<i64> = std::collections::HashSet::new();
        // Family anchors: articles carrying any linked AI tag.
        if !rule.family_ids.is_empty() {
            let ph = vec!["?"; rule.family_ids.len()].join(",");
            let sql = format!(
                "SELECT DISTINCT at.article_id FROM article_tags at
                 JOIN tags t ON t.id = at.tag_id
                 WHERE t.kind = ?1 AND at.tag_id IN ({ph})"
            );
            let mut args: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::Text(TAG_KIND_AI.to_string())];
            args.extend(
                rule.family_ids
                    .iter()
                    .map(|i| rusqlite::types::Value::Integer(*i)),
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
                r.get::<_, i64>(0)
            })?;
            for id in rows {
                candidates.insert(id?);
            }
        }
        // Text scan (positive keywords only — exclusions are applied below to
        // every candidate, matching rule_matches' "any keyword, vetoed by any
        // exclusion" semantics).
        if !rule.definition.keywords.is_empty() {
            let expr = "lower(coalesce(a.title,'') || ' ' || \
                        coalesce(nullif(trim(a.summary),''), substr(a.body_text,1,800), ''))";
            let clauses: Vec<String> = rule
                .definition
                .keywords
                .iter()
                .map(|_| format!("instr({expr}, ?) > 0"))
                .collect();
            let sql = format!(
                "SELECT DISTINCT a.id FROM articles a WHERE {}",
                clauses.join(" OR ")
            );
            let binds: Vec<String> = rule
                .definition
                .keywords
                .iter()
                .map(|k| k.to_lowercase())
                .collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                r.get::<_, i64>(0)
            })?;
            for id in rows {
                candidates.insert(id?);
            }
        }

        // Skip what is already on the shelf; honor exclusions; then (optionally)
        // attach.
        let mut count = 0usize;
        for article_id in &candidates {
            if attached.contains(article_id) {
                continue;
            }
            if !rule.definition.exclude.is_empty() {
                let text = article_text(conn, *article_id)?;
                if lower_contains(&text, &rule.definition.exclude) {
                    continue;
                }
            }
            if apply {
                db::set_article_tag(conn, *article_id, rule.tag_id, true)?;
                attached.insert(*article_id);
            }
            count += 1;
            matched_all.insert(*article_id);
        }
        would_add.push((rule.tag_name.clone(), count));
    }
    Ok(BackfillReport {
        rules: rules.len(),
        would_add,
        distinct_articles: matched_all.len(),
        applied: apply,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::models::TAG_KIND_AI;

    fn in_memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        db::migrate_connection(&mut conn).unwrap();
        conn
    }

    fn insert_feed(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO feeds(feed_url, title) VALUES ('u1','F1')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_article(conn: &Connection, feed_id: i64, title: &str, summary: &str) -> i64 {
        conn.execute(
            "INSERT INTO articles(feed_id, guid, title, summary)
             VALUES (?1, ?2, ?3, ?4)",
            params![feed_id, title, title, summary],
        )
        .unwrap();
        conn.last_insert_rowid()
    }
    /// Create the shelf tag and attach a definition JSON (as migration v36
    /// would for an existing install).
    fn seed_shelf_rule(conn: &Connection, json: &str) -> i64 {
        let id = db::create_tag(
            conn,
            "AI、芯片及算力相关",
            crate::models::TAG_KIND_INTEREST,
        )
        .unwrap();
        conn.execute(
            "UPDATE tags SET definition = ?1 WHERE id = ?2",
            params![json, id],
        )
        .unwrap();
        id
    }

    #[test]
    fn family_anchor_matches_via_subtree() {
        let conn = in_memory_db();
        let shelf = seed_shelf_rule(&conn, r#"{"keywords":["芯片"],"aiTopics":["AI","AI模型"]}"#);
        let ai = db::create_tag(&conn, "AI", TAG_KIND_AI).unwrap();
        let model = db::create_tag(&conn, "AI模型", TAG_KIND_AI).unwrap();
        let deepseek = db::create_tag(&conn, "DeepSeek", TAG_KIND_AI).unwrap();
        db::set_tag_parent(&conn, deepseek, Some(model)).unwrap();
        let feed = insert_feed(&conn);
        // Tagged DeepSeek (child of AI模型) -> family hit via subtree.
        let a1 = insert_article(&conn, feed, "Model update", "no keywords here");
        db::set_article_tag(&conn, a1, deepseek, true).unwrap();
        // No AI tags, but the title keyword hits.
        let a2 = insert_article(&conn, feed, "TSMC 芯片出口管制", "");
        let rules = load_rules(&conn).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rule_matches(&conn, &rules[0], a1).unwrap());
        assert!(rule_matches(&conn, &rules[0], a2).unwrap());
        assert_eq!(attach_for_article(&conn, a1).unwrap(), 1);
        assert_eq!(attach_for_article(&conn, a2).unwrap(), 1);
        // Idempotent: already on the shelf -> nothing more.
        assert_eq!(attach_for_article(&conn, a1).unwrap(), 0);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM article_tags WHERE tag_id = ?1",
                params![shelf],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn exclude_vetoes_and_unrelated_stays_out() {
        let conn = in_memory_db();
        seed_shelf_rule(&conn, r#"{"keywords":["AI"],"exclude":["recall"],"aiTopics":[]}"#);
        let feed = insert_feed(&conn);
        let a1 = insert_article(&conn, feed, "AI car recall widens", "");
        let a2 = insert_article(&conn, feed, "美联储议息会议", "");
        let rules = load_rules(&conn).unwrap();
        assert!(!rule_matches(&conn, &rules[0], a1).unwrap());
        assert!(!rule_matches(&conn, &rules[0], a2).unwrap());
        assert_eq!(attach_for_article(&conn, a1).unwrap(), 0);
    }

    #[test]
    fn backfill_counts_then_applies() {
        let conn = in_memory_db();
        seed_shelf_rule(&conn, r#"{"keywords":["芯片","半导体"],"aiTopics":[]}"#);
        let feed = insert_feed(&conn);
        let _a1 = insert_article(&conn, feed, "三星扩大半导体投资", "");
        let _a2 = insert_article(&conn, feed, "市场综述", "");
        let dry = backfill(&conn, false).unwrap();
        assert_eq!(dry.distinct_articles, 1);
        assert_eq!(dry.would_add[0].1, 1);
        let applied = backfill(&conn, true).unwrap();
        assert_eq!(applied.distinct_articles, 1);
        let again = backfill(&conn, false).unwrap();
        assert_eq!(again.distinct_articles, 0, "second dry-run must find nothing new");
    }
}
