//! Lightweight word-cloud aggregation over article titles/summaries.
//!
//! English words (`[a-z0-9]{2,}`) plus overlapping CJK bigrams, with a shared
//! stopword list. Ported in spirit from FeedOverflow's Go wordcloud package —
//! rewritten in Rust, not linked to Go.

use chrono::{Local, NaiveDate, TimeZone};
use regex::Regex;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Default number of terms returned in the cloud (not articles scanned).
pub const DEFAULT_TOP_N: usize = 100;
/// Hard cap on terms returned in the cloud.
pub const MAX_TOP_N: usize = 120;
/// Soft safety ceiling on articles loaded for aggregation. The date window is
/// already bounded (presets ≤7 days, custom ≤90 days); this only guards huge
/// DBs. Prefer raising it over silently truncating typical ranges.
pub const MAX_SCAN_ROWS: i64 = 100_000;
pub const MAX_SUMMARY_RUN: usize = 400;

static EN_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]{2,}").unwrap());

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub term: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct TextSnippet {
    pub title: String,
    pub summary: String,
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
    /// Number of article rows actually loaded and tokenized for this range
    /// (not the term count). May be less than the true match count only when
    /// [`MAX_SCAN_ROWS`] is hit.
    pub scanned: i64,
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

fn is_cjk(r: char) -> bool {
    matches!(r, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Extract English words and overlapping CJK bigrams from `s`.
pub fn tokenize(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();

    let lower = s.to_lowercase();
    for m in EN_WORD_RE.find_iter(&lower) {
        let w = m.as_str();
        if !is_stopword(w) {
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
            if !is_stopword(&term) {
                out.push(term);
            }
        } else {
            for k in 0..run.len() - 1 {
                let term: String = run[k..k + 2].iter().collect();
                if !is_stopword(&term) {
                    out.push(term);
                }
            }
        }
        i = j;
    }
    out
}

/// Aggregate token counts across snippets; return top N by count (then term).
pub fn aggregate(snippets: &[TextSnippet], top_n: usize) -> Vec<Term> {
    let mut top_n = top_n;
    if top_n == 0 {
        top_n = DEFAULT_TOP_N;
    }
    if top_n > MAX_TOP_N {
        top_n = MAX_TOP_N;
    }
    let mut counts: HashMap<String, i64> = HashMap::new();
    for sn in snippets {
        let summary: String = sn.summary.chars().take(MAX_SUMMARY_RUN).collect();
        let text = format!("{} {}", sn.title, summary);
        for tok in tokenize(&text) {
            *counts.entry(tok).or_insert(0) += 1;
        }
    }
    let mut terms: Vec<Term> = counts
        .into_iter()
        .map(|(term, count)| Term { term, count })
        .collect();
    terms.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.term.cmp(&b.term)));
    terms.truncate(top_n);
    terms
}

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
        let to_exclusive_day = to_day
            .succ_opt()
            .ok_or(RangeError::InvalidTo)?;
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

/// Load article snippets in `[from_ms, to_ms)` and aggregate terms.
///
/// Scans every matching article up to [`MAX_SCAN_ROWS`] (newest first). The
/// returned [`CloudResult::scanned`] is the number of rows actually used —
/// distinct from `terms.len()`, which is capped by `top_n` / [`DEFAULT_TOP_N`].
pub fn build_for_range(
    conn: &Connection,
    range: &Range,
    top_n: usize,
) -> crate::error::AppResult<CloudResult> {
    // published_at / fetched_at are stored as text; compare via unixepoch where possible.
    let mut stmt = conn.prepare(
        "SELECT a.title, COALESCE(a.summary, substr(a.body_text, 1, 400), '')
         FROM articles a
         WHERE (
             CASE
               WHEN a.published_at IS NOT NULL AND a.published_at != ''
                 THEN CAST(strftime('%s', a.published_at) AS INTEGER) * 1000
               ELSE CAST(strftime('%s', a.fetched_at) AS INTEGER) * 1000
             END
           ) >= ?1
           AND (
             CASE
               WHEN a.published_at IS NOT NULL AND a.published_at != ''
                 THEN CAST(strftime('%s', a.published_at) AS INTEGER) * 1000
               ELSE CAST(strftime('%s', a.fetched_at) AS INTEGER) * 1000
             END
           ) < ?2
         ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC
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
        terms: aggregate(&snippets, top_n),
        scanned,
    })
}

fn is_stopword(term: &str) -> bool {
    STOPWORDS.contains(term)
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
        let now = loc
            .with_ymd_and_hms(2026, 8, 4, 15, 30, 0)
            .unwrap();
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

    #[test]
    fn build_for_range_scanned_is_article_count_not_term_cap() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE articles (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                summary TEXT,
                body_text TEXT,
                published_at TEXT,
                fetched_at TEXT
            );",
        )
        .unwrap();
        // 150 articles in range — well above DEFAULT_TOP_N (100 terms).
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
        // Outside the window — must not be counted.
        conn.execute(
            "INSERT INTO articles (title, summary, published_at, fetched_at)
             VALUES ('Old', 'old', '2026-07-01T12:00:00Z', '2026-07-01T12:00:00Z')",
            [],
        )
        .unwrap();

        let loc = FixedOffset::east_opt(0).unwrap();
        let now = loc.with_ymd_and_hms(2026, 8, 4, 15, 0, 0).unwrap();
        let range = resolve_range(1, "", "", now).unwrap();
        let cloud = build_for_range(&conn, &range, DEFAULT_TOP_N).unwrap();
        assert_eq!(cloud.scanned, 150, "scanned must be article rows, not term cap");
        assert!(
            cloud.terms.len() <= DEFAULT_TOP_N,
            "terms stay capped at top_n"
        );
        assert!(!cloud.terms.is_empty());
    }
}
