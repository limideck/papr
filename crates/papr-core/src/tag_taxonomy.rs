//! Tag taxonomy governance: repair the fragmentation of the free-form AI tag
//! vocabulary.
//!
//! The auto-tag worker lets the LLM invent tags, so over time one topic
//! scatters across dozens of near-synonyms — `中东` / `中东局势` / `中东问题`
//! / `Middle East` / `中东冲突` — and related entities (`伊朗`, `以色列`) sit
//! flat beside the regional topic with no hierarchy. This module provides a
//! one-off / periodic **tidy** pass that:
//!
//! 1. **Inventories** the vocabulary (how many tags, how many are one-hit).
//! 2. **Deterministically pre-merges** obvious surface variants (case,
//!    whitespace, hyphenation, punctuation) with no LLM call.
//! 3. Asks the LLM, in small **context-rich batches** (each tag carries sample
//!    article titles), to propose synonym groups *and* a shallow parent
//!    hierarchy — crucially distinguishing a **merge** (`中东局势` ≡ `中东`)
//!    from a **nest** (`伊朗` is an entity *under* the `中东` topic, not the
//!    same concept).
//! 4. Applies the approved plan: merge variants onto a canonical tag while
//!    pinning every old spelling into `tag_aliases` (so the worker reuses the
//!    survivor and the fragmentation never regrows), then set `tag_type` and
//!    `parent_id`.
//!
//! The LLM produces a *plan* only — nothing mutates until [`apply_plan`] runs,
//! so an operator (or an agent) can review/edit the JSON before applying.
//!
//! All behaviour builds on the existing `tags` / `article_tags` /
//! `tag_aliases` tables and the v32 `parent_id` / `tag_type` columns; no new
//! tables and no external search/embedding services.

use crate::ai::{self, AiConfig, TokenUsage};
use crate::db;
use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Output token cap for a clustering batch. The output is a compact JSON plan;
/// generous headroom for ~60 tags of groups/parents.
const PLAN_MAX_TOKENS: u32 = 4000;
/// How many sample article titles to attach to each tag for disambiguation.
const SAMPLE_TITLES: i64 = 3;

// ───────────────────────────── plan types ─────────────────────────────

/// A proposed synonym group: `members` are all folded onto `canonical`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    /// The surviving display name (chosen by the LLM or the highest-usage tag).
    pub canonical: String,
    /// Every surface spelling to merge into the canonical. May include the
    /// canonical itself; it is ignored when merging.
    pub members: Vec<String>,
    /// `entity` (person/place/org) or `topic` (abstract subject), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_type: Option<String>,
    /// Where this group came from: `"deterministic"` (string-rule) or `"llm"`.
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "llm".to_string()
}

/// A proposed hierarchy link for a tag that is NOT merged into anything: it
/// gets a semantic `tag_type` and/or a broader `parent` topic name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hierarchy {
    /// The tag name to position.
    pub name: String,
    /// `entity` | `topic`, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_type: Option<String>,
    /// The canonical name of the broader parent tag (resolved by name at apply
    /// time, after merges). `None` = top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// The full tidy plan. Serialized to JSON for review between `plan` and
/// `apply`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyPlan {
    /// Tag kind this plan was built for (`"ai"` normally).
    pub kind: String,
    /// Minimum article count a tag needed to be considered by the LLM.
    #[serde(default)]
    pub min_count: i64,
    /// How many tags were sent to the LLM (after deterministic grouping).
    #[serde(default)]
    pub considered_tags: usize,
    /// Every merge to perform (deterministic + LLM).
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Every hierarchy/type assignment to perform.
    #[serde(default)]
    pub hierarchy: Vec<Hierarchy>,
    /// Brand-new level-1 topic names to create before hierarchy links are
    /// applied (proposed by the LLM under `--auto-parents`). Each is created
    /// as a `topic`-typed, top-level tag of the plan's kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents_to_create: Vec<String>,
}

/// Summary returned after applying a plan.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub merged_groups: usize,
    pub merged_tags: usize,
    pub articles_repointed: usize,
    pub aliases_pinned: usize,
    pub hierarchy_set: usize,
    pub parents_created: usize,
    /// Member/parent names that could not be resolved (typos, already merged).
    pub skipped: Vec<String>,
}

/// High-level inventory of a tag vocabulary.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyStats {
    pub kind: String,
    pub total_tags: i64,
    pub tags_with_articles: i64,
    /// Tags attached to exactly one article (the long tail).
    pub one_hit_tags: i64,
    /// Tags already folded under a parent.
    pub tags_with_parent: i64,
    /// Distinct parent topics in use.
    pub parent_topics: i64,
    /// Usage histogram: article-count bucket -> number of tags.
    pub histogram: Vec<(String, i64)>,
}

// ───────────────────────────── inventory ─────────────────────────────

/// Compute inventory statistics for `kind` (or both kinds when `None`).
pub fn stats(conn: &Connection, kind: Option<&str>) -> AppResult<TaxonomyStats> {
    let norm = match kind {
        Some(k) => Some(db::normalize_tag_kind(k)?),
        None => None,
    };
    let kind_str = norm.unwrap_or("all").to_string();

    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags WHERE (?1 IS NULL OR kind = ?1)",
        params![norm],
        |r| r.get(0),
    )?;
    let with_articles: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT t.id) FROM tags t
         JOIN article_tags at ON at.tag_id = t.id
         WHERE (?1 IS NULL OR t.kind = ?1)",
        params![norm],
        |r| r.get(0),
    )?;
    let one_hit: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags t
         WHERE (?1 IS NULL OR t.kind = ?1)
           AND (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id) = 1",
        params![norm],
        |r| r.get(0),
    )?;
    let with_parent: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tags
         WHERE parent_id IS NOT NULL AND (?1 IS NULL OR kind = ?1)",
        params![norm],
        |r| r.get(0),
    )?;
    let parent_topics: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT parent_id) FROM tags
         WHERE parent_id IS NOT NULL AND (?1 IS NULL OR kind = ?1)",
        params![norm],
        |r| r.get(0),
    )?;

    // Histogram buckets by article count.
    let mut histogram: Vec<(String, i64)> = Vec::new();
    let buckets: &[(&str, &str)] = &[
        ("1", "= 1"),
        ("2-4", "BETWEEN 2 AND 4"),
        ("5-19", "BETWEEN 5 AND 19"),
        ("20-99", "BETWEEN 20 AND 99"),
        ("100+", ">= 100"),
    ];
    for (label, cond) in buckets {
        let n: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM tags t
                 WHERE (?1 IS NULL OR t.kind = ?1)
                   AND (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id) {cond}"
            ),
            params![norm],
            |r| r.get(0),
        )?;
        histogram.push((label.to_string(), n));
    }

    Ok(TaxonomyStats {
        kind: kind_str,
        total_tags: total,
        tags_with_articles: with_articles,
        one_hit_tags: one_hit,
        tags_with_parent: with_parent,
        parent_topics,
        histogram,
    })
}

// ───────────────────────── deterministic pre-merge ─────────────────────────────

/// Case/punctuation/whitespace-insensitive key for grouping obvious surface
/// variants. Lowercases (Unicode-aware) and keeps only alphanumerics, so
/// `AI`/`ai`, `Middle East`/`middle-east`/`MiddleEast`, and `中东局势`/`中东 局势`
/// collapse together. CJK punctuation (，。、…) is non-alphanumeric and dropped.
fn surface_key(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// A single tag considered during planning.
#[derive(Debug, Clone)]
struct Candidate {
    id: i64,
    name: String,
    count: i64,
    samples: Vec<String>,
    /// Already nested under a parent (level-2). Protected: never merged away
    /// nor re-nested by the tidy pass.
    nested: bool,
    /// Has children of its own (a level-1 parent). Protected from merges and
    /// from being nested deeper (the taxonomy is a two-level forest).
    is_parent: bool,
}

/// Load tags of `kind` with article counts and sample titles.
fn load_candidates(conn: &Connection, kind: &str) -> AppResult<Vec<Candidate>> {
    let usages = db::list_tag_usage(conn, Some(kind))?;
    // Level-2 tags (already nested) and level-1 tags (already parents) are
    // structural: the plan must never merge them away or nest them deeper.
    let nested_ids: std::collections::HashSet<i64> = usages
        .iter()
        .filter(|u| u.parent_id.is_some())
        .map(|u| u.id)
        .collect();
    let parent_ids: std::collections::HashSet<i64> = conn
        .prepare("SELECT DISTINCT parent_id FROM tags WHERE kind = ?1 AND parent_id IS NOT NULL")?
        .query_map(params![kind], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .collect();
    let mut out = Vec::with_capacity(usages.len());
    for u in usages {
        let samples = sample_titles(conn, u.id, SAMPLE_TITLES)?;
        out.push(Candidate {
            id: u.id,
            name: u.name,
            count: u.article_count,
            samples,
            nested: nested_ids.contains(&u.id),
            is_parent: parent_ids.contains(&u.id),
        });
    }
    Ok(out)
}

fn sample_titles(conn: &Connection, tag_id: i64, n: i64) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT a.title FROM articles a
         JOIN article_tags at ON at.article_id = a.id
         WHERE at.tag_id = ?1 AND trim(a.title) != ''
         ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![tag_id, n], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Group candidates whose [`surface_key`] matches. Returns (groups, leftover).
/// Groups with ≥2 members become deterministic merge proposals; singletons are
/// returned for the LLM stage.
fn deterministic_groups(cands: &[Candidate]) -> (Vec<Group>, Vec<&Candidate>) {
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<String, Vec<&Candidate>> = BTreeMap::new();
    for c in cands {
        // Structural tags (already a parent, or already a child) never take
        // part in merges: merging a parent away would detach its subtree, and
        // merging a positioned child would lose its hierarchy link.
        if c.nested || c.is_parent {
            continue;
        }
        let key = surface_key(&c.name);
        if key.is_empty() {
            continue;
        }
        buckets.entry(key).or_default().push(c);
    }
    let mut groups = Vec::new();
    let mut grouped_ids = std::collections::HashSet::new();
    for (_key, members) in buckets {
        if members.len() < 2 {
            continue;
        }
        // Canonical = highest-usage member (ties: first by count desc, then id).
        let mut sorted: Vec<&&Candidate> = members.iter().collect();
        sorted.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
        let canonical = sorted[0].name.clone();
        let member_names: Vec<String> = members.iter().map(|c| c.name.clone()).collect();
        for m in &members {
            grouped_ids.insert(m.id);
        }
        groups.push(Group {
            canonical,
            members: member_names,
            tag_type: None,
            source: "deterministic".to_string(),
        });
    }
    let leftover: Vec<&Candidate> = cands
        .iter()
        .filter(|c| !grouped_ids.contains(&c.id))
        .collect();
    (groups, leftover)
}

// ───────────────────────────── LLM clustering ─────────────────────────────

/// Build a tidy plan for `kind`: deterministic groups first, then LLM batches
/// over the remaining tags with at least `min_count` articles.
///
/// `max_tags` bounds how many leftovers are sent to the model (highest-usage
/// first); `batch_size` is tags per LLM call. Token usage is recorded to the
/// `ai_usage` ledger under feature `"tag-tidy"`.
///
/// With `auto_parents`, a hierarchy parent may reference any existing level-1
/// topic (the model sees a candidate pool) or propose a fresh level-1 topic.
/// After the LLM pass, [`reconcile_parents`] canonicalises every proposal:
/// existing tags win, surface variants fold together, and a new parent
/// survives only when ≥ 2 distinct tags attach to it. Survivors land in
/// `TaxonomyPlan.parents_to_create`, which apply creates before nesting
/// children.
pub async fn build_plan(
    conn: &Connection,
    client: &reqwest::Client,
    cfg: &AiConfig,
    kind: &str,
    min_count: i64,
    max_tags: usize,
    batch_size: usize,
    auto_parents: bool,
) -> AppResult<TaxonomyPlan> {
    let kind = db::normalize_tag_kind(kind)?;
    let cands = load_candidates(conn, kind)?;
    let (det_groups, leftover) = deterministic_groups(&cands);

    // Candidate pool of existing level-1 topics the model may nest under.
    let pool: Vec<String> = if auto_parents {
        load_parent_pool(conn, &kind, PARENT_POOL_MAX)?
    } else {
        Vec::new()
    };

    // Only LLM-cluster tags above the usage floor; structural tags (already a
    // child, or already a parent) are never offered to the model; the long
    // tail stays out of both the prompt and the plan (it is hidden by UI count
    // thresholds, not merged).
    let mut targets: Vec<&Candidate> = leftover
        .into_iter()
        .filter(|c| c.count >= min_count && !c.nested && !c.is_parent)
        .collect();
    // Highest-usage first, cap total, then chunk.
    targets.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
    targets.truncate(max_tags);

    let mut plan = TaxonomyPlan {
        kind: kind.to_string(),
        min_count,
        considered_tags: targets.len(),
        groups: det_groups,
        hierarchy: Vec::new(),
        parents_to_create: Vec::new(),
    };

    let mut total_usage = TokenUsage::default();
    for batch in targets.chunks(batch_size.max(1)) {
        let (system, user) = clustering_prompt(batch, &pool);
        let outcome = ai::complete_chat_json(client, cfg, &system, &user, PLAN_MAX_TOKENS).await?;
        total_usage += outcome.usage;
        match parse_clustering_response(&outcome.text, batch) {
            Ok((mut groups, mut hierarchy)) => {
                plan.groups.append(&mut groups);
                plan.hierarchy.append(&mut hierarchy);
            }
            Err(e) => {
                // A single bad batch shouldn't sink the whole run; skip it and
                // surface the gap via logging. The deterministic groups remain.
                log::warn!("tag-tidy: skipping a batch with unparseable output: {e}");
            }
        }
    }

    if auto_parents {
        reconcile_parents(conn, &kind, &mut plan.hierarchy, &mut plan.parents_to_create)?;
    }

    db::record_ai_usage(conn, "tag-tidy", cfg.provider_name(), cfg.model(), total_usage)?;
    Ok(plan)
}

/// How many existing level-1 topic names to show the model as candidate
/// parents per prompt (highest usage first). Bounds prompt size.
const PARENT_POOL_MAX: usize = 600;

/// Existing level-1 parent candidates: top-level tags of `kind` that are typed
/// `topic`, already have children, or carry enough usage to plausibly act as
/// an umbrella. `entity`-typed tags never become parents.
fn load_parent_pool(conn: &Connection, kind: &str, max_names: usize) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.name FROM tags t
         WHERE t.kind = ?1 AND t.parent_id IS NULL
           AND t.tag_type != 'entity'
           AND (t.tag_type = 'topic'
                OR EXISTS(SELECT 1 FROM tags c WHERE c.parent_id = t.id)
                OR (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id) >= 50)
         ORDER BY (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id) DESC, t.name
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![kind, max_names as i64], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Canonicalise raw LLM parent proposals into names the plan can apply.
///
/// * A proposal matching an existing top-level topic (exact, then surface)
///   is rewritten to the live tag's real name — apply nests under that tag
///   instead of duplicating it.
/// * Surface variants of a brand-new parent fold onto one canonical spelling;
///   a new parent survives only when at least two distinct tags attach to it
///   (the vocabulary already has enough singleton parents).
/// * Anything else (entity parents, self-parents, singletons) is dropped from
///   `parent` — the child keeps its type and stays level-1.
fn reconcile_parents(
    conn: &Connection,
    kind: &str,
    hierarchy: &mut [Hierarchy],
    parents_to_create: &mut Vec<String>,
) -> AppResult<()> {
    // Every existing tag of this kind: (name, surface, acceptable, usage).
    // "Acceptable" as a parent = top-level and not entity-typed. An existing
    // tag that is NOT acceptable still shadows its name: a proposal matching it
    // must be dropped, not re-created as a duplicate tag.
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id),
                t.parent_id, t.tag_type
         FROM tags t
         WHERE t.kind = ?1",
    )?;
    let existing: Vec<(String, String, bool, i64)> = stmt
        .query_map(params![kind], |r| {
            let name: String = r.get(1)?;
            Ok((
                name.clone(),
                surface_key(&name),
                r.get::<_, Option<i64>>(3)?.is_none()
                    && r.get::<_, Option<String>>(4)?.as_deref() != Some("entity"),
                r.get(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Snapshot the raw proposals before rewriting in place.
    let proposed: Vec<Option<String>> = hierarchy.iter().map(|h| h.parent.clone()).collect();

    let mut assignment: Vec<Option<String>> = vec![None; hierarchy.len()]; // chosen parent per entry
    let mut new_buckets: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new(); // surface key -> proposed spellings
    let mut children: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new(); // surface key -> distinct child surfaces

    for (idx, p) in proposed.iter().enumerate() {
        let Some(p) = p.as_deref() else { continue };
        let p = p.trim();
        let key = surface_key(p);
        let child_key = surface_key(&hierarchy[idx].name);
        if key.is_empty() || key == child_key {
            continue; // blank or self-parent
        }
        // Exact, then surface (highest usage), against ALL existing tags.
        let hit = existing
            .iter()
            .find(|(n, _, _, _)| n.eq_ignore_ascii_case(p))
            .or_else(|| {
                existing
                    .iter()
                    .filter(|(_, s, _, _)| *s == key)
                    .max_by(|a, b| a.3.cmp(&b.3).then(a.0.cmp(&b.0)))
            });
        if let Some((name, _, acceptable, _)) = hit {
            // Existing tag: usable as a parent only when acceptable; otherwise
            // the proposal is dropped (child keeps its type, stays level-1).
            if *acceptable {
                assignment[idx] = Some(name.clone());
            }
        } else {
            new_buckets.entry(key.clone()).or_default().push(p.to_string());
            children.entry(key).or_default().insert(child_key);
        }
    }

    // New parents need >= 2 distinct children; canonical = most-proposed
    // spelling (ties: shortest, then lexicographic).
    let mut new_canonical: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (key, bucket) in &new_buckets {
        if children.get(key).map(|s| s.len()).unwrap_or(0) < 2 {
            continue;
        }
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for name in bucket {
            *counts.entry(name.clone()).or_default() += 1;
        }
        let canonical = counts
            .iter()
            .max_by(|a, b| {
                a.1.cmp(b.1)
                    .then_with(|| b.0.len().cmp(&a.0.len()))
                    .then_with(|| a.0.cmp(b.0))
            })
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| bucket[0].clone());
        new_canonical.insert(key.clone(), canonical);
    }

    // Rewrite in place.
    for (idx, h) in hierarchy.iter_mut().enumerate() {
        let Some(p) = h.parent.as_deref() else { continue };
        let key = surface_key(p);
        let chosen = assignment[idx]
            .clone()
            .or_else(|| new_canonical.get(&key).cloned().filter(|n| n != &h.name));
        h.parent = chosen;
    }

    *parents_to_create = new_canonical.into_values().collect();
    parents_to_create.sort();
    Ok(())
}

fn clustering_prompt(batch: &[&Candidate], pool: &[String]) -> (String, String) {
    let system = "You are organising a news reader's tag taxonomy. \
Reply with ONLY a JSON object, no markdown, no commentary. \
The input is a list of tags, each with its article count and a few sample article titles. \
Do TWO things: \
1) 'groups': cluster tags that name the SAME concept (synonyms, bilingual variants, \
abbreviations, or near-duplicate phrasings — e.g. 中东 / 中东局势 / 中东问题 / Middle East / \
中东冲突 are ONE concept). Choose the clearest, most standard 'canonical' name and list every \
variant in 'members'. \
2) 'hierarchy': for tags that are NOT synonyms but belong together, assign a parent. \
CRITICAL DISTINCTION: a country/person/organisation (type 'entity') is NOT the same concept as \
the regional topic it relates to — do NOT merge 伊朗 into 中东. Instead put 伊朗 in 'hierarchy' \
with type 'entity' and parent '中东局势'. Abstract subjects use type 'topic'. \
Keep the hierarchy shallow (parent = a broad topic/region, or null for top level). \
When unsure whether two tags are the same concept, prefer SEPARATE (hierarchy or leave alone) \
over a wrong merge — merging is hard to undo. \
Only reference tag names present in the input. Output this exact shape: \
{\"groups\":[{\"canonical\":\"中东局势\",\"type\":\"topic\",\"members\":[\"中东\",\"中东问题\",\"Middle East\"]}], \
\"hierarchy\":[{\"name\":\"伊朗\",\"type\":\"entity\",\"parent\":\"中东局势\"}]}";
    let mut system = system.to_string();

    let mut user_obj = serde_json::json!({ "tags": batch.iter().map(|c| {
        serde_json::json!({
            "name": c.name,
            "count": c.count,
            "samples": c.samples,
        })
    }).collect::<Vec<_>>() });

    if !pool.is_empty() {
        // The pool is bounded by PARENT_POOL_MAX; pass it verbatim and tell
        // the model to prefer it over inventing new level-1 topics.
        system.push_str(
            " The 'parents' array lists real level-1 topics that already exist in this vocabulary. \
For a 'hierarchy' entry whose tag clearly belongs under one of them, use that exact name as 'parent'. \
Only when NOTHING in 'parents' fits may you propose a concise new level-1 topic name as 'parent' — \
it must be a generic umbrella (a region, sector or theme), never the tag's own name, never a \
person/company/brand, and it will be created as a brand-new top-level topic. Prefer 'parents' entries \
over new proposals. A tag that is itself a broad level-1 theme should get 'parent': null.",
        );
        user_obj["parents"] = serde_json::Value::Array(pool.iter().map(|s| serde_json::Value::String(s.clone())).collect());
    }

    (system, user_obj.to_string())
}

/// Parse the model's JSON into groups + hierarchy, constraining every name to
/// tags present in this batch (so a hallucinated name never enters the plan).
fn parse_clustering_response(
    text: &str,
    batch: &[&Candidate],
) -> AppResult<(Vec<Group>, Vec<Hierarchy>)> {
    // Reuse the robust JSON-object extractor (strips fences / reasoning
    // wrappers, takes the last balanced object).
    let obj_str = crate::auto_tag::extract_json_object(text)
        .ok_or_else(|| AppError::other("tag-tidy: no JSON object in model output"))?;
    let root: serde_json::Value = serde_json::from_str(&obj_str)
        .map_err(|e| AppError::other(format!("tag-tidy: invalid JSON: {e}")))?;

    let known = |name: &str| -> Option<&Candidate> {
        batch
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
            .copied()
    };

    let mut groups = Vec::new();
    if let Some(arr) = root.get("groups").and_then(|v| v.as_array()) {
        for g in arr {
            let canonical = g
                .get("canonical")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let raw_members: Vec<&str> = g
                .get("members")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str()).collect())
                .unwrap_or_default();
            let tag_type = g
                .get("type")
                .and_then(|v| v.as_str())
                .map(normalize_type)
                .flatten();

            // Resolve every member to a known tag, dedup by id.
            let mut ids: Vec<i64> = Vec::new();
            let mut names: Vec<String> = Vec::new();
            for m in raw_members {
                if let Some(cand) = known(m) {
                    if !ids.contains(&cand.id) {
                        ids.push(cand.id);
                        names.push(cand.name.clone());
                    }
                }
            }
            // A valid group needs >=2 distinct known members.
            if ids.len() < 2 {
                continue;
            }
            // Canonical must be one of the known members; else take the
            // highest-usage member as canonical.
            let canonical = if known(&canonical).is_some() {
                canonical
            } else {
                let top = batch
                    .iter()
                    .filter(|c| ids.contains(&c.id))
                    .max_by(|a, b| a.count.cmp(&b.count))
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| names[0].clone());
                top
            };
            groups.push(Group {
                canonical,
                members: names,
                tag_type,
                source: "llm".to_string(),
            });
        }
    }

    let mut hierarchy = Vec::new();
    if let Some(arr) = root.get("hierarchy").and_then(|v| v.as_array()) {
        for h in arr {
            let name = h
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if known(&name).is_none() {
                continue;
            }
            let tag_type = h
                .get("type")
                .and_then(|v| v.as_str())
                .map(normalize_type)
                .flatten();
            let parent = h
                .get("parent")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            // Only keep an entry that actually sets something.
            if tag_type.is_some() || parent.is_some() {
                hierarchy.push(Hierarchy {
                    name,
                    tag_type,
                    parent,
                });
            }
        }
    }

    Ok((groups, hierarchy))
}

fn normalize_type(s: &str) -> Option<String> {
    match s.trim().to_lowercase().as_str() {
        "entity" | "entities" | "person" | "place" | "org" | "organization" => {
            Some("entity".to_string())
        }
        "topic" | "subject" | "theme" => Some("topic".to_string()),
        _ => None,
    }
}

// ───────────────────────────── apply ─────────────────────────────

/// Resolve a tag id by (kind, name) case-insensitively against the live DB.
fn tag_id_by_name(conn: &Connection, kind: &str, name: &str) -> AppResult<Option<i64>> {
    let id = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND name = ?2 COLLATE NOCASE",
            params![kind, name.trim()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    Ok(id)
}

/// Apply a tidy plan: merge groups (pinning old spellings as aliases so the
/// fragmentation can't regrow), then set types and parents.
///
/// Merges run first so hierarchy parents referenced by a canonical name
/// resolve to the surviving tags. Each merge is its own transaction.
pub fn apply_plan(conn: &Connection, plan: &TaxonomyPlan) -> AppResult<ApplyReport> {
    let kind = db::normalize_tag_kind(&plan.kind)?;
    let mut report = ApplyReport::default();

    // 1) Merges.
    for g in &plan.groups {
        let canonical = g.canonical.trim();
        if canonical.is_empty() {
            report.skipped.push(format!("(empty canonical group)"));
            continue;
        }
        // Resolve or create the survivor.
        let survivor = match tag_id_by_name(conn, kind, canonical)? {
            Some(id) => id,
            None => db::create_tag(conn, canonical, kind)?,
        };

        // Fold every distinct member that exists and isn't the survivor.
        let mut merged_in_group = 0usize;
        for member_name in &g.members {
            let m = member_name.trim();
            if m.is_empty() {
                continue;
            }
            // NOTE: compare by resolved id, not by name — the members list
            // includes the canonical itself, often in a different case
            // ("middle east" vs canonical "Middle East"), and a name-based
            // skip would wrongly swallow the variant we actually want to
            // merge.
            let Some(mid) = tag_id_by_name(conn, kind, m)? else {
                // Already merged into something in an earlier group, or a typo.
                report.skipped.push(m.to_string());
                continue;
            };
            if mid == survivor {
                continue;
            }
            match db::merge_tags_keep_alias(conn, mid, survivor, &[]) {
                Ok(moved) => {
                    report.articles_repointed += moved;
                    merged_in_group += 1;
                }
                Err(e) => {
                    report.skipped.push(format!("{m} ({})", e));
                }
            }
        }
        if merged_in_group > 0 {
            report.merged_groups += 1;
            report.merged_tags += merged_in_group;
        }
        // Type the survivor if the plan specified one.
        if let Some(t) = &g.tag_type {
            let _ = db::set_tag_type(conn, survivor, Some(t));
        }
    }

    // 2) Auto-created level-1 parents (before links, so children can resolve).
    for parent_name in &plan.parents_to_create {
        let name = parent_name.trim();
        if name.is_empty() {
            continue;
        }
        // A spelling already pinned as an alias of another tag would shadow
        // that tag if we created a fresh row; skip instead.
        let alias_hit: Option<i64> = conn
            .query_row(
                "SELECT tag_id FROM tag_aliases
                 WHERE kind = ?1 AND alias = ?2 COLLATE NOCASE LIMIT 1",
                params![kind, name],
                |r| r.get(0),
            )
            .optional()?;
        match tag_id_by_name(conn, kind, name)? {
            // Already exists (a live tag or a duplicate proposal that resolved
            // to one). Leave its type alone — never silently re-type a tag the
            // operator or a previous pass already positioned.
            Some(_id) => {}
            None if alias_hit.is_some() => {
                report
                    .skipped
                    .push(format!("new parent {name} (name is an alias)"));
            }
            None => {
                let id = db::create_tag(conn, name, kind)?;
                db::set_tag_type(conn, id, Some("topic"))?;
                report.parents_created += 1;
            }
        }
    }

    // 3) Hierarchy / types (names resolve post-merge + post-parents, so links
    //    land on the live tags).
    for h in &plan.hierarchy {
        let Some(tag_id) = tag_id_by_name(conn, kind, &h.name)? else {
            report.skipped.push(h.name.clone());
            continue;
        };
        // The tag must be a movable leaf of THIS vocabulary: not already
        // nested (the tree is two levels) and without children of its own
        // (that would open a third level). Positioned tags keep their type.
        let (child_parent, child_kids): (Option<i64>, i64) = conn.query_row(
            "SELECT parent_id, (SELECT COUNT(*) FROM tags c WHERE c.parent_id = t.id)
             FROM tags t WHERE t.id = ?1",
            params![tag_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if child_parent.is_some() {
            report
                .skipped
                .push(format!("{} (already nested)", h.name));
            continue;
        }
        if child_kids > 0 {
            report.skipped.push(format!(
                "{} (has children; no third level)",
                h.name
            ));
            continue;
        }
        if let Some(t) = &h.tag_type {
            db::set_tag_type(conn, tag_id, Some(t))?;
        }
        let Some(parent_name) = h.parent.as_deref() else {
            // Type-only assignment still counts as positioned.
            if h.tag_type.is_some() {
                report.hierarchy_set += 1;
            }
            continue;
        };
        let Some(pid) = tag_id_by_name(conn, kind, parent_name)? else {
            report.skipped.push(format!(
                "{} (parent {} missing)",
                h.name, parent_name
            ));
            continue;
        };
        if pid == tag_id {
            report.skipped.push(format!("{} self-parent", h.name));
            continue;
        }
        // Parent sanity: not an entity, and itself level-1 (a parent of the
        // parent would make a third level).
        let (p_type, p_parent): (Option<String>, Option<i64>) = conn.query_row(
            "SELECT tag_type, parent_id FROM tags WHERE id = ?1",
            params![pid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if p_type.as_deref() == Some("entity") {
            report.skipped.push(format!(
                "{} (parent {} is an entity)",
                h.name, parent_name
            ));
            continue;
        }
        if p_parent.is_some() {
            report.skipped.push(format!(
                "{} (parent {} already nested)",
                h.name, parent_name
            ));
            continue;
        }
        db::set_tag_parent(conn, tag_id, Some(pid))?;
        report.hierarchy_set += 1;
    }

    // Aliases pinned = aliases now pointing at survivors. Reported as a rough
    // count for operator visibility; exact delta is not tracked per-merge.
    let alias_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tag_aliases WHERE kind = ?1",
        params![kind],
        |r| r.get(0),
    )?;
    report.aliases_pinned = alias_count as usize;

    Ok(report)
}

/// Load AI config from the same settings the auto-tag worker uses.
pub fn load_ai_config(conn: &Connection) -> AppResult<AiConfig> {
    AiConfig::new(
        db::get_setting(conn, "ai_provider")?,
        db::get_setting(conn, "ai_api_key")?,
        db::get_setting(conn, "ai_model")?,
        db::get_setting(conn, "ai_base_url")?,
    )
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TAG_KIND_AI;

    #[test]
    fn surface_key_collapses_variants() {
        assert_eq!(surface_key("AI"), surface_key("ai"));
        assert_eq!(surface_key("Middle East"), surface_key("middle-east"));
        assert_eq!(surface_key("Middle East"), surface_key("MiddleEast"));
        assert_eq!(surface_key("中东局势"), surface_key("中东 局势"));
        assert_eq!(surface_key("中东，局势"), surface_key("中东局势"));
        // Distinct concepts stay distinct.
        assert_ne!(surface_key("中东"), surface_key("伊朗"));
        assert_ne!(surface_key("AI"), surface_key("AIM"));
    }

    #[test]
    fn normalize_type_maps_variants() {
        assert_eq!(normalize_type("Entity").as_deref(), Some("entity"));
        assert_eq!(normalize_type("PLACE").as_deref(), Some("entity"));
        assert_eq!(normalize_type("topic").as_deref(), Some("topic"));
        assert_eq!(normalize_type("gibberish"), None);
    }

    #[test]
    fn parse_response_constrains_to_known_tags() {
        let batch = vec![
            make_cand(1, "中东局势", 40),
            make_cand(2, "中东", 30),
            make_cand(3, "Middle East", 10),
            make_cand(4, "伊朗", 25),
            make_cand(5, "幻觉标签", 1),
        ];
        let batch_refs: Vec<&Candidate> = batch.iter().collect();
        let text = r#"{"groups":[
            {"canonical":"中东局势","type":"topic","members":["中东","Middle East","中东局势","不存在的标签"]}
          ],"hierarchy":[
            {"name":"伊朗","type":"entity","parent":"中东局势"},
            {"name":"幻觉标签","type":"entity","parent":"幽灵"}
          ]}"#;
        let (groups, hierarchy) = parse_clustering_response(text, &batch_refs).unwrap();
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.canonical, "中东局势");
        // 3 known members, hallucinated "不存在的标签" dropped.
        assert_eq!(g.members.len(), 3);
        assert_eq!(g.tag_type.as_deref(), Some("topic"));
        // Hierarchy: 伊朗 kept with a resolvable parent; 幻觉标签 references an
        // unknown parent but still carries a type, so it is kept (the parent is
        // re-validated at apply time).
        assert_eq!(hierarchy.len(), 2);
        let iran = hierarchy.iter().find(|h| h.name == "伊朗").unwrap();
        assert_eq!(iran.parent.as_deref(), Some("中东局势"));
        assert_eq!(iran.tag_type.as_deref(), Some("entity"));
    }

    #[test]
    fn parse_response_ignores_non_json() {
        let batch = vec![make_cand(1, "AI", 5)];
        let batch_refs: Vec<&Candidate> = batch.iter().collect();
        assert!(parse_clustering_response("sorry, I cannot help", &batch_refs).is_err());
    }

    #[test]
    fn deterministic_grouping_merges_only_surface_variants() {
        let cands = vec![
            make_cand(1, "Middle East", 40),
            make_cand(2, "middle east", 30),
            make_cand(3, "中东冲突", 20),
            make_cand(4, "伊朗", 15),
        ];
        let (groups, leftover) = deterministic_groups(&cands);
        // "Middle East" + "middle east" merge; CJK variants and 伊朗 stay.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical, "Middle East");
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].source, "deterministic");
        assert_eq!(leftover.len(), 2);
    }

    #[test]
    fn deterministic_groups_never_touch_structural_tags() {
        // A level-1 parent ("AI") and an already-nested child ("Middle East")
        // each have an un-positioned surface twin. Merging either would detach
        // a subtree or lose a link, so the twins must NOT be folded.
        let cands = vec![
            Candidate {
                id: 1,
                name: "AI".into(),
                count: 100,
                is_parent: true,
                ..Candidate::leaf()
            },
            Candidate {
                id: 2,
                name: "ai".into(),
                count: 2,
                ..Candidate::leaf()
            },
            Candidate {
                id: 3,
                name: "Middle East".into(),
                count: 90,
                nested: true,
                ..Candidate::leaf()
            },
            Candidate {
                id: 4,
                name: "middle-east".into(),
                count: 3,
                ..Candidate::leaf()
            },
            make_cand(5, "伊朗", 25),
        ];
        let (groups, leftover) = deterministic_groups(&cands);
        assert!(groups.is_empty(), "structural twins must not merge");
        assert_eq!(leftover.len(), 5);
    }

    #[test]
    fn reconcile_parents_prefers_existing_and_folds_new_surfaces() {
        let conn = in_memory_db();
        let ai = db::create_tag(&conn, "AI", TAG_KIND_AI).unwrap();
        db::set_tag_type(&conn, ai, Some("topic")).unwrap();
        let mut hierarchy = vec![
            Hierarchy {
                name: "OpenAI".into(),
                tag_type: Some("entity".into()),
                parent: Some("ai".into()), // case-variant of existing "AI"
            },
            Hierarchy {
                name: "Anthropic".into(),
                tag_type: Some("entity".into()),
                parent: Some("AI 公司".into()),
            },
            Hierarchy {
                name: "xAI".into(),
                tag_type: Some("entity".into()),
                parent: Some("AI公司".into()), // surface twin of the above
            },
            Hierarchy {
                name: "孤独新父级".into(),
                tag_type: Some("entity".into()),
                parent: Some("单独主题".into()), // only one child -> dropped
            },
        ];
        let mut to_create = Vec::new();
        reconcile_parents(&conn, TAG_KIND_AI, &mut hierarchy, &mut to_create).unwrap();
        // OpenAI folds onto the live "AI" tag (case-insensitive exact match).
        assert_eq!(hierarchy[0].parent.as_deref(), Some("AI"));
        // The two spellings of the new parent fold onto one canonical name.
        assert_eq!(hierarchy[1].parent.as_deref(), hierarchy[2].parent.as_deref());
        assert_eq!(hierarchy[1].parent.as_deref(), Some("AI公司"));
        // Singleton new parent is dropped -> child stays level-1, type kept.
        assert_eq!(hierarchy[3].parent, None);
        assert_eq!(to_create, vec!["AI公司".to_string()]);
    }

    #[test]
    fn apply_creates_missing_parents_then_nests_children() {
        let conn = in_memory_db();
        let openai = db::create_tag(&conn, "OpenAI", TAG_KIND_AI).unwrap();
        let anthropic = db::create_tag(&conn, "Anthropic", TAG_KIND_AI).unwrap();
        let plan = TaxonomyPlan {
            kind: TAG_KIND_AI.to_string(),
            parents_to_create: vec!["AI公司".into()],
            hierarchy: vec![
                Hierarchy {
                    name: "OpenAI".into(),
                    tag_type: Some("entity".into()),
                    parent: Some("AI公司".into()),
                },
                Hierarchy {
                    name: "Anthropic".into(),
                    tag_type: Some("entity".into()),
                    parent: Some("AI公司".into()),
                },
            ],
            ..Default::default()
        };
        let report = apply_plan(&conn, &plan).unwrap();
        assert_eq!(report.parents_created, 1);
        assert_eq!(report.hierarchy_set, 2);
        let parent_id = tag_id_by_name(&conn, TAG_KIND_AI, "AI公司").unwrap().unwrap();
        let (p_type, p_parent): (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT tag_type, parent_id FROM tags WHERE id = ?1",
                params![parent_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(p_type.as_deref(), Some("topic"));
        assert!(p_parent.is_none());
        let child_parent: Option<i64> = conn
            .query_row(
                "SELECT parent_id FROM tags WHERE id = ?1",
                params![openai],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(child_parent, Some(parent_id));
        let second_parent: Option<i64> = conn
            .query_row(
                "SELECT parent_id FROM tags WHERE id = ?1",
                params![anthropic],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(second_parent, Some(parent_id));
        // Children and the new parent are all distinct rows.
        assert_ne!(openai, parent_id);
        assert_ne!(anthropic, parent_id);
        assert_ne!(openai, anthropic);
    }

    #[test]
    fn apply_refuses_third_level_and_entity_parents() {
        let conn = in_memory_db();
        let topic = db::create_tag(&conn, "tech", TAG_KIND_AI).unwrap();
        db::set_tag_type(&conn, topic, Some("topic")).unwrap();
        let mid = db::create_tag(&conn, "AI infrastructure", TAG_KIND_AI).unwrap();
        db::set_tag_parent(&conn, mid, Some(topic)).unwrap();
        let leaf = db::create_tag(&conn, "NVIDIA", TAG_KIND_AI).unwrap();
        let company = db::create_tag(&conn, "OpenAI", TAG_KIND_AI).unwrap();
        db::set_tag_type(&conn, company, Some("entity")).unwrap();
        let victim = db::create_tag(&conn, "xAI", TAG_KIND_AI).unwrap();

        // leaf -> mid would be a third level (mid is already nested);
        // victim -> company would nest under an entity.
        let plan = TaxonomyPlan {
            kind: TAG_KIND_AI.to_string(),
            hierarchy: vec![
                Hierarchy {
                    name: "NVIDIA".into(),
                    tag_type: Some("entity".into()),
                    parent: Some("AI infrastructure".into()),
                },
                Hierarchy {
                    name: "xAI".into(),
                    tag_type: Some("entity".into()),
                    parent: Some("OpenAI".into()),
                },
            ],
            ..Default::default()
        };
        let report = apply_plan(&conn, &plan).unwrap();
        assert_eq!(report.hierarchy_set, 0);
        assert_eq!(report.skipped.len(), 2);
        assert!(report.skipped.iter().any(|s| s.contains("already nested")));
        assert!(report.skipped.iter().any(|s| s.contains("is an entity")));
        let p: Option<i64> = conn
            .query_row("SELECT parent_id FROM tags WHERE id = ?1", params![leaf], |r| r.get(0))
            .unwrap();
        assert!(p.is_none());
        let _ = victim;
    }

    fn make_cand(id: i64, name: &str, count: i64) -> Candidate {
        Candidate {
            id,
            name: name.to_string(),
            count,
            samples: vec![],
            ..Candidate::leaf()
        }
    }

    impl Candidate {
        fn leaf() -> Self {
            Candidate {
                id: 0,
                name: String::new(),
                count: 0,
                samples: vec![],
                nested: false,
                is_parent: false,
            }
        }
    }

    // End-to-end DB behaviour: surface variants (punctuation / hyphenation that
    // SQLite's ASCII `COLLATE NOCASE` does NOT fold, e.g. "Middle-East" vs
    // "Middle East") merge, and the old spelling is pinned as an alias so it
    // resolves on the next auto-tag call.
    #[test]
    fn merge_pins_alias_and_resolves_afterward() {
        let conn = in_memory_db();
        // Two distinct surface-variant AI tags that coexist (hyphen vs space is
        // not folded by COLLATE NOCASE), one article on the lower-usage variant.
        let keep = db::create_tag(&conn, "Middle East", TAG_KIND_AI).unwrap();
        let variant = db::create_tag(&conn, "Middle-East", TAG_KIND_AI).unwrap();
        assert_ne!(keep, variant);
        let feed = insert_feed(&conn);
        // Keep-tag has more articles so it wins canonical selection; the
        // hyphen variant has one article.
        let a1 = insert_article(&conn, feed, "Middle East tensions rise");
        let a2 = insert_article(&conn, feed, "Middle East summit held");
        let a3 = insert_article(&conn, feed, "Middle-East crisis deepens");
        db::set_article_tag(&conn, a1, keep, true).unwrap();
        db::set_article_tag(&conn, a2, keep, true).unwrap();
        db::set_article_tag(&conn, a3, variant, true).unwrap();

        // Build a deterministic plan and apply.
        let cands = load_candidates(&conn, TAG_KIND_AI).unwrap();
        let (groups, _) = deterministic_groups(&cands);
        assert_eq!(groups.len(), 1);
        let plan = TaxonomyPlan {
            kind: TAG_KIND_AI.to_string(),
            groups,
            ..Default::default()
        };
        let report = apply_plan(&conn, &plan).unwrap();
        assert_eq!(report.merged_tags, 1);

        // The variant is gone as a tag...
        assert!(tag_id_by_name(&conn, TAG_KIND_AI, "Middle-East")
            .unwrap()
            .is_none());
        // ...but survives as an alias pinned to the canonical tag, so the next
        // auto-tag emission of "Middle-East" reuses the survivor instead of
        // recreating a fragment.
        let alias_target = db::resolve_tag_alias(&conn, TAG_KIND_AI, "Middle-East")
            .unwrap()
            .unwrap();
        assert_eq!(alias_target, keep);
        let resolved = db::resolve_tag_by_name_or_alias(&conn, TAG_KIND_AI, "Middle-East")
            .unwrap()
            .unwrap();
        assert_eq!(resolved, keep);
    }

    fn in_memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrate_connection(&mut conn).unwrap();
        conn
    }

    fn insert_feed(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO feeds(feed_url, title) VALUES ('u1', 'F1')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_article(conn: &Connection, feed_id: i64, title: &str) -> i64 {
        conn.execute(
            "INSERT INTO articles(feed_id, guid, title) VALUES (?1, ?2, ?3)",
            params![feed_id, title, title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }
}
