//! Lightweight word-cloud aggregation over article titles/summaries.
//!
//! English words (`[a-z0-9]{2,}`) plus overlapping CJK bigrams, with a shared
//! stopword list. Ported in spirit from FeedOverflow's Go wordcloud package —
//! rewritten in Rust, not linked to Go.
//!
//! Hot path: terms are tokenized once at ingest into `article_terms`, then the
//! API aggregates with SQL over a calendar-day window. When a gazetteer is
//! present, stored surfaces are remapped to the current canonical at query
//! time (so promoting `ai` → `AI` updates the cloud without waiting for
//! backfill). Mid-migration / empty windows fall back to the legacy
//! scan+tokenize path.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::error::AppResult;
use crate::wordcloud_dict::{self, EntityGroup, WordCloudDict};

/// Default number of terms returned in the cloud (not articles scanned).
pub const DEFAULT_TOP_N: usize = 100;
/// Hard cap on terms returned in the cloud.
pub const MAX_TOP_N: usize = 120;
/// Soft safety ceiling on articles loaded for aggregation. The date window is
/// already bounded (presets ≤7 days, custom ≤90 days); this only guards huge
/// DBs. Prefer raising it over silently truncating typical ranges.
pub const MAX_SCAN_ROWS: i64 = 100_000;
pub const MAX_SUMMARY_RUN: usize = 400;

/// Settings key: monotonically increasing version bumped when the shared
/// stopwords/entities dictionary is reloaded. Articles whose
/// `article_term_index.dict_version` lags need a backfill rebuild.
pub const DICT_VERSION_KEY: &str = "wordcloud_terms_dict_version";
/// Last seen `stopwords.version:entities.version` fingerprint from the shared
/// JSON files — used to bump [`DICT_VERSION_KEY`] only when files change.
pub const DICT_FILE_VERSION_KEY: &str = "wordcloud_dict_file_version";

/// Batch size for backfill workers (fetch → tokenize off-lock → write).
pub const BACKFILL_BATCH: usize = 64;

/// TTL for in-process preset cloud cache (1/3/7 day windows).
const PRESET_CACHE_TTL: Duration = Duration::from_secs(45);

static EN_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]{2,}").unwrap());

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub term: String,
    pub count: i64,
    /// Entity/category group for colouring (`country`, `person`, …).
    pub group: String,
}

#[derive(Debug, Clone)]
pub struct TextSnippet {
    pub title: String,
    pub summary: String,
}

/// One weighted term extracted from a single article snippet.
#[derive(Debug, Clone)]
pub struct ExtractedTerm {
    pub term: String,
    pub group: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    /// Inclusive calendar start `YYYY-MM-DD`.
    pub from: String,
    /// Inclusive calendar end `YYYY-MM-DD`.
    pub to: String,
    /// Window start as Unix ms (inclusive).
    pub from_ms: i64,
    /// Window end as Unix ms (exclusive).
    pub to_ms: i64,
}

/// Aggregated word-cloud for a date range.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudResult {
    pub terms: Vec<Term>,
    /// Number of article rows actually used for this range (not the term
    /// count). May be less than the true match count only when
    /// [`MAX_SCAN_ROWS`] is hit.
    pub scanned: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillStatus {
    pub dict_version: i64,
    pub indexed: i64,
    pub stale: i64,
    pub missing: i64,
    pub total_articles: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillBatchResult {
    pub processed: usize,
    pub remaining: i64,
    pub dict_version: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum RangeError {
    #[error("invalid_from")]
    InvalidFrom,
    #[error("invalid_to")]
    InvalidTo,
    #[error("invalid_range")]
    InvalidRange,
    #[error("invalid_range_too_long")]
    RangeTooLong,
}

// ─── preset TTL cache ───────────────────────────────────────────────────

#[derive(Clone)]
struct CacheEntry {
    at: Instant,
    from: String,
    to: String,
    result: CloudResult,
}

static PRESET_CACHE: LazyLock<Mutex<HashMap<(i32, usize), CacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop all cached preset clouds (call after dict reload or backfill write).
pub fn invalidate_cache() {
    if let Ok(mut guard) = PRESET_CACHE.lock() {
        guard.clear();
    }
}

fn cache_get(days: i32, top_n: usize) -> Option<(String, String, CloudResult)> {
    if !matches!(days, 1 | 3 | 7) {
        return None;
    }
    let guard = PRESET_CACHE.lock().ok()?;
    let entry = guard.get(&(days, top_n))?;
    if entry.at.elapsed() > PRESET_CACHE_TTL {
        return None;
    }
    Some((entry.from.clone(), entry.to.clone(), entry.result.clone()))
}

fn cache_put(days: i32, top_n: usize, range: &Range, result: &CloudResult) {
    if !matches!(days, 1 | 3 | 7) {
        return;
    }
    if let Ok(mut guard) = PRESET_CACHE.lock() {
        guard.insert(
            (days, top_n),
            CacheEntry {
                at: Instant::now(),
                from: range.from.clone(),
                to: range.to.clone(),
                result: result.clone(),
            },
        );
    }
}

// ─── tokenize / extract ─────────────────────────────────────────────────

fn is_cjk(r: char) -> bool {
    matches!(r, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Extract English words and overlapping CJK bigrams from `s` (builtin stopwords only).
pub fn tokenize(s: &str) -> Vec<String> {
    tokenize_with(s, None)
}

fn tokenize_with(s: &str, dict: Option<&WordCloudDict>) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();

    let lower = s.to_lowercase();
    for m in EN_WORD_RE.find_iter(&lower) {
        let w = m.as_str();
        if !is_filtered(w, dict) {
            out.push(w.to_string());
        }
    }

    let runes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < runes.len() {
        if !is_cjk(runes[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < runes.len() && is_cjk(runes[j]) {
            j += 1;
        }
        let run = &runes[i..j];
        if run.len() == 1 {
            let term: String = run.iter().collect();
            if !is_filtered(&term, dict) {
                out.push(term);
            }
        } else {
            for k in 0..run.len() - 1 {
                let term: String = run[k..k + 2].iter().collect();
                if !is_filtered(&term, dict) {
                    out.push(term);
                }
            }
        }
        i = j;
    }
    out
}

#[derive(Default)]
struct Freq {
    count: i64,
    group: EntityGroup,
    text: String,
}

/// Aggregate token counts across snippets; return top N by count (then term).
pub fn aggregate(snippets: &[TextSnippet], top_n: usize) -> Vec<Term> {
    aggregate_with(snippets, top_n, None)
}

/// Aggregate with optional entity gazetteer + file stopwords.
pub fn aggregate_with(
    snippets: &[TextSnippet],
    top_n: usize,
    dict: Option<&WordCloudDict>,
) -> Vec<Term> {
    let top_n = clamp_top_n(top_n);
    let mut freq: HashMap<String, Freq> = HashMap::new();
    for sn in snippets {
        let summary: String = sn.summary.chars().take(MAX_SUMMARY_RUN).collect();
        let text = format!("{} {}", sn.title, summary);
        count_text(&text, dict, &mut freq);
    }
    finish_freq(freq, top_n)
}

fn clamp_top_n(mut top_n: usize) -> usize {
    if top_n == 0 {
        top_n = DEFAULT_TOP_N;
    }
    if top_n > MAX_TOP_N {
        top_n = MAX_TOP_N;
    }
    top_n
}

fn finish_freq(freq: HashMap<String, Freq>, top_n: usize) -> Vec<Term> {
    let mut terms: Vec<Term> = freq
        .into_values()
        .filter(|f| !f.text.is_empty())
        .map(|f| Term {
            term: f.text,
            count: f.count,
            group: f.group.as_str().to_string(),
        })
        .collect();
    terms.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.term.cmp(&b.term)));
    terms.truncate(top_n);
    terms
}

fn count_text(text: &str, dict: Option<&WordCloudDict>, freq: &mut HashMap<String, Freq>) {
    for et in terms_for_text(text, dict) {
        let key = format!("{}|{}", et.group, et.term);
        let entry = freq.entry(key).or_default();
        entry.count += et.weight.round() as i64;
        if entry.count < 1 {
            entry.count = 1;
        }
        entry.group = EntityGroup::parse_loose(&et.group);
        entry.text = et.term;
    }
}

/// Tokenize + entity-match one article text blob into weighted terms.
///
/// Shared by ingest indexing, backfill, and the legacy scan fallback.
pub fn terms_for_text(text: &str, dict: Option<&WordCloudDict>) -> Vec<ExtractedTerm> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let mut freq: HashMap<String, ExtractedTerm> = HashMap::new();

    if let Some(dict) = dict {
        let (hits, occupied, norm) = dict.match_entities(text);
        for (id, n) in hits {
            if n <= 0 {
                continue;
            }
            let Some(ent) = dict.entity(&id) else {
                continue;
            };
            let key = format!("e:{id}");
            let entry = freq.entry(key).or_insert_with(|| ExtractedTerm {
                term: ent.canonical.clone(),
                group: ent.group.as_str().to_string(),
                weight: 0.0,
            });
            entry.weight += n as f64;
        }
        for span in wordcloud_dict::unoccupied_spans(&norm, &occupied) {
            for tok in tokenize_with(&span, Some(dict)) {
                let entry = freq.entry(tok.clone()).or_insert_with(|| ExtractedTerm {
                    term: tok.clone(),
                    group: EntityGroup::General.as_str().to_string(),
                    weight: 0.0,
                });
                entry.weight += 1.0;
            }
        }
        return freq.into_values().collect();
    }

    for tok in tokenize_with(text, None) {
        let entry = freq.entry(tok.clone()).or_insert_with(|| ExtractedTerm {
            term: tok.clone(),
            group: EntityGroup::General.as_str().to_string(),
            weight: 0.0,
        });
        entry.weight += 1.0;
    }
    freq.into_values().collect()
}

/// Build terms for one article's title + summary (summary already truncated).
pub fn terms_for_snippet(
    title: &str,
    summary: &str,
    dict: Option<&WordCloudDict>,
) -> Vec<ExtractedTerm> {
    let summary: String = summary.chars().take(MAX_SUMMARY_RUN).collect();
    let text = format!("{} {}", title, summary);
    terms_for_text(&text, dict)
}

// ─── day helpers ────────────────────────────────────────────────────────

/// Local calendar day `YYYY-MM-DD` for an article's effective timestamp,
/// aligned with [`resolve_range`]'s local-day windows.
pub fn effective_day_local(published_at: Option<&str>, fetched_at: Option<&str>) -> String {
    let raw = published_at
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| fetched_at.map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or("");
    if raw.is_empty() {
        return Local::now().format("%Y-%m-%d").to_string();
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return dt.with_timezone(&Local).format("%Y-%m-%d").to_string();
    }
    // SQLite `datetime('now')` / space-separated form.
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        return Local
            .from_utc_datetime(&naive)
            .format("%Y-%m-%d")
            .to_string();
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Local
            .from_utc_datetime(&naive)
            .format("%Y-%m-%d")
            .to_string();
    }
    if raw.len() >= 10 {
        if let Ok(d) = NaiveDate::parse_from_str(&raw[..10], "%Y-%m-%d") {
            return d.format("%Y-%m-%d").to_string();
        }
    }
    Local::now().format("%Y-%m-%d").to_string()
}

// ─── dict version + persistence ─────────────────────────────────────────

pub fn current_dict_version(conn: &Connection) -> AppResult<i64> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![DICT_VERSION_KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// Bump the stored dict version (call after stopwords/entities reload).
/// Does not delete existing terms — backfill rebuilds stale articles.
pub fn bump_dict_version(conn: &Connection) -> AppResult<i64> {
    let next = current_dict_version(conn)?.saturating_add(1);
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![DICT_VERSION_KEY, next.to_string()],
    )?;
    invalidate_cache();
    Ok(next)
}

/// After a dict reload, bump the terms dict version only when the on-disk
/// stopwords/entities `version` fields changed. Returns the (possibly new)
/// terms dict version and whether a bump happened.
pub fn sync_dict_file_version(conn: &Connection, dict: &WordCloudDict) -> AppResult<(i64, bool)> {
    let fingerprint = format!("{}:{}", dict.stopwords.version, dict.entities.version);
    let prev: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![DICT_FILE_VERSION_KEY],
            |r| r.get(0),
        )
        .optional()?;
    if prev.as_deref() == Some(fingerprint.as_str()) {
        return Ok((current_dict_version(conn)?, false));
    }
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![DICT_FILE_VERSION_KEY, fingerprint],
    )?;
    // First sighting seeds the fingerprint without forcing a full rebuild.
    if prev.is_none() {
        let v = ensure_dict_version(conn)?;
        return Ok((v, false));
    }
    let v = bump_dict_version(conn)?;
    Ok((v, true))
}

/// Ensure a non-zero dict version exists (first run / fresh DB).
pub fn ensure_dict_version(conn: &Connection) -> AppResult<i64> {
    let v = current_dict_version(conn)?;
    if v > 0 {
        return Ok(v);
    }
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO NOTHING",
        params![DICT_VERSION_KEY],
    )?;
    current_dict_version(conn)
}

/// Replace all stored terms for one article (idempotent re-index).
///
/// Starts its own transaction. When already inside a caller transaction
/// (e.g. ingest), use [`replace_article_terms_conn`] instead.
pub fn replace_article_terms(
    conn: &Connection,
    article_id: i64,
    day: &str,
    terms: &[ExtractedTerm],
    dict_version: i64,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    replace_article_terms_conn(&tx, article_id, day, terms, dict_version)?;
    tx.commit()?;
    Ok(())
}

/// Like [`replace_article_terms`] but does not open a nested transaction —
/// safe to call from inside `upsert_article`'s write transaction.
pub fn replace_article_terms_conn(
    conn: &Connection,
    article_id: i64,
    day: &str,
    terms: &[ExtractedTerm],
    dict_version: i64,
) -> AppResult<()> {
    conn.execute(
        "DELETE FROM article_terms WHERE article_id = ?1",
        params![article_id],
    )?;
    {
        let mut stmt = conn.prepare(
            "INSERT INTO article_terms(article_id, term, group_key, weight, day)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(article_id, term) DO UPDATE SET
                 group_key = excluded.group_key,
                 weight = excluded.weight,
                 day = excluded.day",
        )?;
        for et in terms {
            if et.term.is_empty() || et.weight <= 0.0 {
                continue;
            }
            stmt.execute(params![article_id, et.term, et.group, et.weight, day])?;
        }
    }
    conn.execute(
        "INSERT INTO article_term_index(article_id, dict_version, updated_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(article_id) DO UPDATE SET
             dict_version = excluded.dict_version,
             updated_at = datetime('now')",
        params![article_id, dict_version],
    )?;
    Ok(())
}

/// Index one article from title/summary snippets already in hand (ingest path).
///
/// Uses the process-wide word-cloud dictionary when `dict` is `None`.
/// `fetched_at` may be `None` on insert (SQLite default); day then falls back
/// to published_at or "today".
pub fn index_article_snippet(
    conn: &Connection,
    article_id: i64,
    title: &str,
    summary: &str,
    published_at: Option<&str>,
    fetched_at: Option<&str>,
) -> AppResult<()> {
    let dict_arc = wordcloud_dict::process_dict();
    dict_arc.with_dict(|dict| {
        let dict_version = ensure_dict_version(conn).unwrap_or(1);
        let day = effective_day_local(published_at, fetched_at);
        let terms = terms_for_snippet(title, summary, Some(dict));
        // No nested transaction — ingest may already hold one.
        replace_article_terms_conn(conn, article_id, &day, &terms, dict_version)
    })
}

/// Index one article by loading its row (backfill path).
pub fn index_article_by_id(conn: &Connection, article_id: i64, dict: &WordCloudDict) -> AppResult<()> {
    let row: Option<(String, String, Option<String>, String)> = conn
        .query_row(
            "SELECT title,
                    COALESCE(summary, substr(body_text, 1, 400), ''),
                    published_at,
                    fetched_at
             FROM articles WHERE id = ?1",
            params![article_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((title, summary, published_at, fetched_at)) = row else {
        return Ok(());
    };
    let dict_version = ensure_dict_version(conn)?;
    let day = effective_day_local(published_at.as_deref(), Some(&fetched_at));
    let terms = terms_for_snippet(&title, &summary, Some(dict));
    replace_article_terms(conn, article_id, &day, &terms, dict_version)?;
    Ok(())
}

/// Articles needing (re)tokenization for the current dict version.
pub fn list_articles_needing_terms(
    conn: &Connection,
    limit: usize,
) -> AppResult<Vec<(i64, String, String, Option<String>, String)>> {
    let dict_version = ensure_dict_version(conn)?;
    let limit = limit.max(1) as i64;
    let mut stmt = conn.prepare(
        "SELECT a.id,
                a.title,
                COALESCE(a.summary, substr(a.body_text, 1, 400), ''),
                a.published_at,
                a.fetched_at
         FROM articles a
         LEFT JOIN article_term_index i ON i.article_id = a.id
         WHERE i.article_id IS NULL OR i.dict_version != ?1
         ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![dict_version, limit], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn backfill_status(conn: &Connection) -> AppResult<BackfillStatus> {
    let dict_version = ensure_dict_version(conn)?;
    let total_articles: i64 =
        conn.query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))?;
    let indexed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM article_term_index WHERE dict_version = ?1",
        params![dict_version],
        |r| r.get(0),
    )?;
    let stale: i64 = conn.query_row(
        "SELECT COUNT(*) FROM article_term_index WHERE dict_version != ?1",
        params![dict_version],
        |r| r.get(0),
    )?;
    let missing = (total_articles - indexed - stale).max(0);
    Ok(BackfillStatus {
        dict_version,
        indexed,
        stale,
        missing,
        total_articles,
    })
}

/// Process up to `limit` articles missing/stale terms.
///
/// Intended for the background worker: caller should fetch rows under the DB
/// lock, tokenize **without** holding the lock, then call
/// [`write_backfill_batch`] — or use this convenience when the lock is cheap.
pub fn backfill_batch(
    conn: &Connection,
    dict: &WordCloudDict,
    limit: usize,
) -> AppResult<BackfillBatchResult> {
    let rows = list_articles_needing_terms(conn, limit)?;
    let dict_version = ensure_dict_version(conn)?;
    let mut prepared = Vec::with_capacity(rows.len());
    for (id, title, summary, published_at, fetched_at) in &rows {
        let day = effective_day_local(published_at.as_deref(), Some(fetched_at));
        let terms = terms_for_snippet(title, summary, Some(dict));
        prepared.push((*id, day, terms));
    }
    write_backfill_batch(conn, dict_version, &prepared)?;
    let status = backfill_status(conn)?;
    let remaining = status.missing + status.stale;
    Ok(BackfillBatchResult {
        processed: prepared.len(),
        remaining,
        dict_version,
    })
}

/// Write a pre-tokenized backfill batch (tokenize happened off the DB lock).
pub fn write_backfill_batch(
    conn: &Connection,
    dict_version: i64,
    prepared: &[(i64, String, Vec<ExtractedTerm>)],
) -> AppResult<()> {
    for (id, day, terms) in prepared {
        replace_article_terms(conn, *id, day, terms, dict_version)?;
    }
    if !prepared.is_empty() {
        invalidate_cache();
    }
    Ok(())
}

/// Fetch a batch of articles needing terms (hold DB lock only for this).
pub fn fetch_backfill_batch(
    conn: &Connection,
    limit: usize,
) -> AppResult<(i64, Vec<(i64, String, String, Option<String>, String)>)> {
    let dict_version = ensure_dict_version(conn)?;
    let rows = list_articles_needing_terms(conn, limit)?;
    Ok((dict_version, rows))
}

/// Tokenize a fetched backfill batch without touching the DB.
pub fn tokenize_backfill_batch(
    rows: &[(i64, String, String, Option<String>, String)],
    dict: &WordCloudDict,
) -> Vec<(i64, String, Vec<ExtractedTerm>)> {
    rows.iter()
        .map(|(id, title, summary, published_at, fetched_at)| {
            let day = effective_day_local(published_at.as_deref(), Some(fetched_at));
            let terms = terms_for_snippet(title, summary, Some(dict));
            (*id, day, terms)
        })
        .collect()
}

// ─── range resolve ──────────────────────────────────────────────────────

/// Build a time window from `days` (1|3|7) or `from`/`to` calendar dates.
/// When both from and to are present they win. `to` is inclusive.
pub fn resolve_range(
    days: i32,
    from_str: &str,
    to_str: &str,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Result<Range, RangeError> {
    let loc = now.timezone();

    if !from_str.is_empty() && !to_str.is_empty() {
        let from_day = NaiveDate::parse_from_str(from_str, "%Y-%m-%d")
            .map_err(|_| RangeError::InvalidFrom)?;
        let to_day =
            NaiveDate::parse_from_str(to_str, "%Y-%m-%d").map_err(|_| RangeError::InvalidTo)?;
        if to_day < from_day {
            return Err(RangeError::InvalidRange);
        }
        if (to_day - from_day).num_days() > 90 {
            return Err(RangeError::RangeTooLong);
        }
        let from_dt = loc
            .from_local_datetime(&from_day.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .ok_or(RangeError::InvalidFrom)?;
        let to_exclusive_day = to_day.succ_opt().ok_or(RangeError::InvalidTo)?;
        let to_dt = loc
            .from_local_datetime(&to_exclusive_day.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .ok_or(RangeError::InvalidTo)?;
        return Ok(Range {
            from: from_day.format("%Y-%m-%d").to_string(),
            to: to_day.format("%Y-%m-%d").to_string(),
            from_ms: from_dt.timestamp_millis(),
            to_ms: to_dt.timestamp_millis(),
        });
    }

    let days = if matches!(days, 1 | 3 | 7) { days } else { 1 };
    let end_day = now.date_naive();
    let from_day = end_day - chrono::Duration::days((days - 1) as i64);
    let to_exclusive = end_day.succ_opt().unwrap();
    let from_dt = loc
        .from_local_datetime(&from_day.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap();
    let to_dt = loc
        .from_local_datetime(&to_exclusive.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap();
    Ok(Range {
        from: from_day.format("%Y-%m-%d").to_string(),
        to: end_day.format("%Y-%m-%d").to_string(),
        from_ms: from_dt.timestamp_millis(),
        to_ms: to_dt.timestamp_millis(),
    })
}

/// Convenience: resolve using the local timezone clock.
pub fn resolve_range_local(days: i32, from_str: &str, to_str: &str) -> Result<Range, RangeError> {
    let now = Local::now().fixed_offset();
    resolve_range(days, from_str, to_str, now)
}

// ─── build cloud ────────────────────────────────────────────────────────

/// Load article snippets in `[from_ms, to_ms)` and aggregate terms.
pub fn build_for_range(
    conn: &Connection,
    range: &Range,
    top_n: usize,
) -> AppResult<CloudResult> {
    build_for_range_with(conn, range, top_n, None)
}

/// Like [`build_for_range`], applying a shared stopwords/entities dictionary.
///
/// Prefers SQL aggregation over `article_terms`, remapping residual surfaces
/// through the gazetteer when `dict` is set. Falls back to the legacy
/// scan+tokenize path when the window has articles but no indexed terms yet
/// (mid-migration / empty backfill).
pub fn build_for_range_with(
    conn: &Connection,
    range: &Range,
    top_n: usize,
    dict: Option<&WordCloudDict>,
) -> AppResult<CloudResult> {
    let top_n = clamp_top_n(top_n);
    if let Some(cloud) = try_build_from_terms(conn, range, top_n, dict)? {
        return Ok(cloud);
    }
    build_for_range_scan(conn, range, top_n, dict)
}

/// Preset-aware entry: serves 1/3/7-day results from a short TTL cache when
/// the resolved window is a 1/3/7-day preset (no custom from/to).
pub fn build_for_range_cached(
    conn: &Connection,
    range: &Range,
    days: i32,
    custom_range: bool,
    top_n: usize,
    dict: Option<&WordCloudDict>,
) -> AppResult<CloudResult> {
    let top_n = clamp_top_n(top_n);
    if !custom_range {
        if let Some((from, to, result)) = cache_get(days, top_n) {
            if range.from == from && range.to == to {
                return Ok(result);
            }
        }
    }
    let cloud = build_for_range_with(conn, range, top_n, dict)?;
    if !custom_range {
        cache_put(days, top_n, range, &cloud);
    }
    Ok(cloud)
}

fn try_build_from_terms(
    conn: &Connection,
    range: &Range,
    top_n: usize,
    dict: Option<&WordCloudDict>,
) -> AppResult<Option<CloudResult>> {
    // Table may be empty mid-migration — detect coverage for this window.
    let terms_articles: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT article_id) FROM article_terms
         WHERE day >= ?1 AND day <= ?2",
        params![range.from, range.to],
        |r| r.get(0),
    )?;
    if terms_articles == 0 {
        // Any articles in the window at all? If yes, fall back to scan.
        let articles_in_range = count_articles_in_range(conn, range)?;
        if articles_in_range > 0 {
            return Ok(None);
        }
        return Ok(Some(CloudResult {
            terms: Vec::new(),
            scanned: 0,
        }));
    }

    // No LIMIT here: remapping aliases → canonical can merge rows that would
    // otherwise fall outside a pre-limit top-N (e.g. residual `ai` + entity `AI`).
    let mut stmt = conn.prepare(
        "SELECT term, group_key, CAST(ROUND(SUM(weight)) AS INTEGER) AS cnt
         FROM article_terms
         WHERE day >= ?1 AND day <= ?2
         GROUP BY term, group_key",
    )?;
    let rows = stmt.query_map(params![range.from, range.to], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;

    let mut freq: HashMap<String, Freq> = HashMap::new();
    for row in rows {
        let (raw_term, raw_group, cnt) = row?;
        if cnt <= 0 || raw_term.is_empty() {
            continue;
        }
        let (term, group) = resolve_stored_term(&raw_term, &raw_group, dict);
        let key = format!("{group}|{term}");
        let entry = freq.entry(key).or_default();
        entry.count = entry.count.saturating_add(cnt);
        entry.group = EntityGroup::parse_loose(&group);
        entry.text = term;
    }

    Ok(Some(CloudResult {
        terms: finish_freq(freq, top_n),
        scanned: terms_articles.min(MAX_SCAN_ROWS),
    }))
}

/// Map a stored `article_terms` surface through the live gazetteer.
///
/// Residual tokens (`ai`) and stale canonicals (`Ai`) resolve to the entity's
/// current display spelling (`AI`) and group so the cloud updates on entity
/// create/edit without waiting for term-index backfill.
fn resolve_stored_term(
    term: &str,
    group: &str,
    dict: Option<&WordCloudDict>,
) -> (String, String) {
    let Some(dict) = dict else {
        return (term.to_string(), group.to_string());
    };
    let Some(syn) = dict.lookup_synonym_group(term) else {
        return (term.to_string(), group.to_string());
    };
    let group = dict
        .entity(&syn.id)
        .map(|e| e.group.as_str().to_string())
        .unwrap_or_else(|| group.to_string());
    (syn.canonical, group)
}

fn count_articles_in_range(conn: &Connection, range: &Range) -> AppResult<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM articles a
         WHERE datetime(COALESCE(a.published_at, a.fetched_at))
               >= datetime(?1 / 1000, 'unixepoch')
           AND datetime(COALESCE(a.published_at, a.fetched_at))
               < datetime(?2 / 1000, 'unixepoch')",
        params![range.from_ms, range.to_ms],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Legacy path: scan title/summary text and tokenize in-process.
fn build_for_range_scan(
    conn: &Connection,
    range: &Range,
    top_n: usize,
    dict: Option<&WordCloudDict>,
) -> AppResult<CloudResult> {
    // Date filter uses the same `datetime(COALESCE(...))` expression as
    // `idx_articles_sort` / list queries — not per-row `strftime('%s', ...)`.
    let mut stmt = conn.prepare(
        "SELECT a.title, COALESCE(a.summary, substr(a.body_text, 1, 400), '')
         FROM articles a
         WHERE datetime(COALESCE(a.published_at, a.fetched_at))
               >= datetime(?1 / 1000, 'unixepoch')
           AND datetime(COALESCE(a.published_at, a.fetched_at))
               < datetime(?2 / 1000, 'unixepoch')
         ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![range.from_ms, range.to_ms, MAX_SCAN_ROWS], |r| {
        Ok(TextSnippet {
            title: r.get(0)?,
            summary: r.get(1)?,
        })
    })?;
    let mut snippets = Vec::new();
    for row in rows {
        snippets.push(row?);
    }
    let scanned = snippets.len() as i64;
    Ok(CloudResult {
        terms: aggregate_with(&snippets, top_n, dict),
        scanned,
    })
}

fn is_filtered(term: &str, dict: Option<&WordCloudDict>) -> bool {
    if STOPWORDS.contains(term) {
        return true;
    }
    dict.is_some_and(|d| d.is_file_stopword(term))
}

impl EntityGroup {
    fn parse_loose(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "country" => Self::Country,
            "person" => Self::Person,
            "location" => Self::Location,
            "military" => Self::Military,
            "politics" => Self::Politics,
            "economy" => Self::Economy,
            "disaster" => Self::Disaster,
            "org" => Self::Org,
            _ => Self::General,
        }
    }
}

/// Common English + Chinese stopwords (lowercase / as emitted by tokenize).
static STOPWORDS: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    const WORDS: &[&str] = &[
        "a", "an", "the", "and", "or", "but", "if", "then", "else", "when", "at", "by", "for",
        "with", "about", "against", "between", "into", "through", "during", "before", "after",
        "above", "below", "to", "from", "up", "down", "in", "out", "on", "off", "over", "under",
        "again", "further", "once", "here", "there", "all", "any", "both", "each", "few", "more",
        "most", "other", "some", "such", "no", "nor", "not", "only", "own", "same", "so", "than",
        "too", "very", "can", "will", "just", "don", "should", "now", "is", "are", "was", "were",
        "be", "been", "being", "have", "has", "had", "having", "do", "does", "did", "doing",
        "would", "could", "ought", "i", "me", "my", "myself", "we", "our", "ours", "ourselves",
        "you", "your", "yours", "he", "him", "his", "she", "her", "hers", "it", "its", "they",
        "them", "their", "what", "which", "who", "whom", "this", "that", "these", "those", "am",
        "of", "as", "how", "why", "where", "while", "also", "via", "per", "vs", "new", "news",
        "says", "said", "may", "one", "two", "first", "last", "year", "years", "day", "days",
        "week", "month", "time", "get", "got", "like", "make", "made", "see", "way", "back",
        "still", "even", "much", "well", "us", "re", "ll", "ve", "d", "s", "t", "m", "http",
        "https", "www", "com", "的", "了", "在", "是", "我", "有", "和", "就", "不", "人", "都",
        "一", "一个", "上", "也", "很", "到", "说", "要", "去", "你", "会", "着", "没有", "看",
        "好", "自己", "这", "那", "他", "她", "它", "们", "为", "与", "及", "或", "而", "被",
        "把", "让", "从", "对", "向", "以", "之", "中", "后", "前", "下", "里", "外", "等",
        "能", "可以", "已经", "还", "又", "再", "更", "最", "比", "却", "但", "如果", "因为",
        "所以", "虽然", "但是", "什么", "怎么", "如何", "这个", "那个", "这些", "那些", "我们",
        "他们", "她们", "它们", "你们", "其", "其中", "以及", "关于", "根据", "通过", "进行",
        "表示", "认为", "目前", "近日", "今日", "昨日", "明天", "今天", "今年", "去年", "日前",
        "记者", "报道", "消息", "称", "称其",
    ];
    WORDS.iter().copied().collect()
});

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn tokenize_english() {
        let toks = tokenize("Bitcoin and Ethereum Rally as Markets Rise");
        let set: std::collections::HashSet<_> = toks.iter().map(String::as_str).collect();
        for want in ["bitcoin", "ethereum", "rally", "markets", "rise"] {
            assert!(set.contains(want), "missing {want} in {toks:?}");
        }
        assert!(!set.contains("and"));
        assert!(!set.contains("as"));
    }

    #[test]
    fn tokenize_chinese_bigrams() {
        let toks = tokenize("中国经济增速");
        let set: std::collections::HashSet<_> = toks.iter().map(String::as_str).collect();
        for want in ["中国", "国经", "经济", "济增", "增速"] {
            assert!(set.contains(want), "missing {want} in {toks:?}");
        }
    }

    #[test]
    fn aggregate_top() {
        let snips = [
            TextSnippet {
                title: "Bitcoin rally".into(),
                summary: "Bitcoin markets rise".into(),
            },
            TextSnippet {
                title: "Bitcoin dips".into(),
                summary: "Ethereum also moves".into(),
            },
            TextSnippet {
                title: "Unrelated weather".into(),
                summary: "rain in seattle".into(),
            },
        ];
        let terms = aggregate(&snips, 5);
        assert_eq!(terms[0].term, "bitcoin");
        assert!(terms[0].count >= 3);
    }

    #[test]
    fn resolve_range_days() {
        let loc = FixedOffset::east_opt(8 * 3600).unwrap();
        let now = loc.with_ymd_and_hms(2026, 8, 4, 15, 30, 0).unwrap();
        let r = resolve_range(1, "", "", now).unwrap();
        assert_eq!(r.from, "2026-08-04");
        assert_eq!(r.to, "2026-08-04");
        let r3 = resolve_range(3, "", "", now).unwrap();
        assert_eq!(r3.from, "2026-08-02");
        assert_eq!(r3.to, "2026-08-04");
    }

    #[test]
    fn resolve_range_custom() {
        let loc = FixedOffset::east_opt(0).unwrap();
        let now = loc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
        let r = resolve_range(1, "2026-07-01", "2026-07-07", now).unwrap();
        assert_eq!(r.from, "2026-07-01");
        assert_eq!(r.to, "2026-07-07");
        let want_to = loc
            .with_ymd_and_hms(2026, 7, 8, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(r.to_ms, want_to);
    }

    fn migrate_memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Minimal schema for term index tests + full migration via open is
        // heavy; create the tables the wordcloud helpers need.
        conn.execute_batch(
            "CREATE TABLE articles (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                summary TEXT,
                body_text TEXT,
                published_at TEXT,
                fetched_at TEXT
            );
            CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE article_terms (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                term TEXT NOT NULL,
                group_key TEXT NOT NULL DEFAULT 'general',
                weight REAL NOT NULL DEFAULT 1,
                day TEXT NOT NULL,
                PRIMARY KEY (article_id, term)
            );
            CREATE TABLE article_term_index (
                article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
                dict_version INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn build_for_range_scanned_is_article_count_not_term_cap() {
        let conn = migrate_memory();
        for i in 0..150 {
            conn.execute(
                "INSERT INTO articles (title, summary, published_at, fetched_at)
                 VALUES (?1, ?2, ?3, ?3)",
                params![
                    format!("Bitcoin article {i}"),
                    "Ethereum markets rally news update",
                    "2026-08-04T12:00:00Z",
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO articles (title, summary, published_at, fetched_at)
             VALUES ('Old', 'old', '2026-07-01T12:00:00Z', '2026-07-01T12:00:00Z')",
            [],
        )
        .unwrap();

        let loc = FixedOffset::east_opt(0).unwrap();
        let now = loc.with_ymd_and_hms(2026, 8, 4, 15, 0, 0).unwrap();
        let range = resolve_range(1, "", "", now).unwrap();
        // No pre-agg yet → scan fallback.
        let cloud = build_for_range(&conn, &range, DEFAULT_TOP_N).unwrap();
        assert_eq!(
            cloud.scanned, 150,
            "scanned must be article rows, not term cap"
        );
        assert!(
            cloud.terms.len() <= DEFAULT_TOP_N,
            "terms stay capped at top_n"
        );
        assert!(!cloud.terms.is_empty());
    }

    #[test]
    fn build_from_preaggregated_terms() {
        let conn = migrate_memory();
        conn.execute(
            "INSERT INTO articles (id, title, summary, published_at, fetched_at)
             VALUES (1, 'Bitcoin rally', 'markets', '2026-08-04T12:00:00Z', '2026-08-04T12:00:00Z')",
            [],
        )
        .unwrap();
        let terms = terms_for_snippet("Bitcoin rally", "Bitcoin markets", None);
        replace_article_terms(&conn, 1, "2026-08-04", &terms, 1).unwrap();

        let loc = FixedOffset::east_opt(0).unwrap();
        let now = loc.with_ymd_and_hms(2026, 8, 4, 15, 0, 0).unwrap();
        let range = resolve_range(1, "", "", now).unwrap();
        let cloud = build_for_range(&conn, &range, DEFAULT_TOP_N).unwrap();
        assert_eq!(cloud.scanned, 1);
        assert!(
            cloud.terms.iter().any(|t| t.term == "bitcoin"),
            "terms={:?}",
            cloud.terms
        );
    }

    #[test]
    fn query_time_remaps_residual_alias_to_canonical() {
        use crate::wordcloud_dict::{EntitiesFile, WordCloudEntity};

        let conn = migrate_memory();
        conn.execute(
            "INSERT INTO articles (id, title, summary, published_at, fetched_at)
             VALUES (1, 'ai boom', 'more ai news', '2026-08-04T12:00:00Z', '2026-08-04T12:00:00Z')",
            [],
        )
        .unwrap();
        // Simulate pre-entity index: residual lowercase stored as-is.
        replace_article_terms(
            &conn,
            1,
            "2026-08-04",
            &[ExtractedTerm {
                term: "ai".into(),
                group: "general".into(),
                weight: 3.0,
            }],
            1,
        )
        .unwrap();

        let mut dict = WordCloudDict::empty(std::env::temp_dir());
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![WordCloudEntity {
                id: "general.ai".into(),
                canonical: "AI".into(),
                group: "general".into(),
                aliases: vec!["ai".into(), "Ai".into()],
            }],
        });

        let loc = FixedOffset::east_opt(0).unwrap();
        let now = loc.with_ymd_and_hms(2026, 8, 4, 15, 0, 0).unwrap();
        let range = resolve_range(1, "", "", now).unwrap();
        let cloud = build_for_range_with(&conn, &range, DEFAULT_TOP_N, Some(&dict)).unwrap();
        let ai = cloud
            .terms
            .iter()
            .find(|t| t.term == "AI")
            .expect("canonical AI after remap");
        assert_eq!(ai.count, 3);
        assert!(
            !cloud.terms.iter().any(|t| t.term == "ai"),
            "residual must not remain: {:?}",
            cloud.terms
        );
    }
}
