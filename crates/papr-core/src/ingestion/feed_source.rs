//! Directory-index feed sources (Apache/nginx "Index of" pages listing `.xml`).
//!
//! Behaviour mirrors FeedOverflow's feed-source scanner: fetch the index HTML,
//! extract `.xml` hrefs, auto-subscribe new feeds, and report stale candidates
//! without deleting them. Reimplemented in Rust — not a copy of the Go source.

use crate::db;
use crate::error::{AppError, AppResult};
use crate::ingestion::{fetch, parse};
use crate::models::SourceType;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use tokio::sync::Mutex;
use url::Url;

static XML_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)href=["']([^"']+\.xml)["']"#).unwrap());

const INDEX_FETCH_LIMIT: usize = 2 << 20; // 2 MiB

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSource {
    pub id: i64,
    pub base_url: String,
    pub last_checked_at: Option<String>,
    /// Sidebar folder auto-created for this index (feeds from the scan land here).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    /// Feeds whose URL is under this index prefix.
    pub feed_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedFeed {
    pub id: i64,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleFeed {
    pub id: i64,
    pub name: String,
    pub url: String,
    /// `missing_from_index` | `fetch_failed`
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub id: i64,
    pub base_url: String,
    pub added: Vec<AddedFeed>,
    pub skipped: usize,
    pub stale: Vec<StaleFeed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Trim and store indexes with a trailing `/`. Empty input → empty string.
pub fn normalize_index_url(raw: &str) -> String {
    let u = raw.trim();
    if u.is_empty() {
        return String::new();
    }
    format!("{}/", u.trim_end_matches('/'))
}

/// Absolute http(s) URL suitable as an index base.
pub fn valid_index_url(raw: &str) -> bool {
    let base = normalize_index_url(raw);
    if base.is_empty() {
        return false;
    }
    let Ok(u) = Url::parse(&base) else {
        return false;
    };
    matches!(u.scheme(), "http" | "https") && u.host().is_some()
}

/// Trim and strip a trailing `/` for stable feed URL dedupe.
pub fn normalize_feed_url(raw: &str) -> String {
    let u = raw.trim();
    if u.is_empty() {
        return String::new();
    }
    u.trim_end_matches('/').to_string()
}

/// Resolve an href against a normalized index base URL.
pub fn resolve_feed_href(base_url: &str, href: &str) -> AppResult<String> {
    let href = href.trim();
    if href.is_empty() {
        return Err(AppError::other("empty href"));
    }
    let base = Url::parse(&normalize_index_url(base_url))?;
    let resolved = base.join(href)?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return Err(AppError::other(format!(
            "unsupported scheme {}",
            resolved.scheme()
        )));
    }
    Ok(normalize_feed_url(resolved.as_str()))
}

/// Raw href values pointing at `.xml` from an HTML body (deduped, order preserved).
pub fn extract_xml_hrefs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for caps in XML_LINK_RE.captures_iter(body) {
        let h = caps[1].to_string();
        if seen.insert(h.clone()) {
            out.push(h);
        }
    }
    out
}

/// Sidebar folder label from an index URL: last non-empty path segment
/// (hyphens/underscores → spaces, title-cased). Falls back to the host when
/// the path is empty — e.g. `…/foreignpolicy/` → `"Foreignpolicy"`.
pub fn folder_name_from_index_url(base_url: &str) -> String {
    let base = normalize_index_url(base_url);
    let Ok(u) = Url::parse(&base) else {
        let trimmed = base.trim_matches('/').to_string();
        return if trimmed.is_empty() {
            "Feeds".into()
        } else {
            title_case_words(&trimmed.replace('-', " ").replace('_', " "))
        };
    };
    let path = u.path().trim_matches('/');
    let raw = if path.is_empty() {
        u.host_str().unwrap_or("Feeds").to_string()
    } else {
        path.rsplit('/').next().unwrap_or(path).to_string()
    };
    let labeled = title_case_words(&raw.replace('-', " ").replace('_', " "));
    if labeled.is_empty() {
        "Feeds".into()
    } else {
        labeled
    }
}

fn title_case_words(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(s.len());
    let mut word_start = true;
    for c in s.chars() {
        if c.is_whitespace() {
            word_start = true;
            out.push(c);
            continue;
        }
        if word_start {
            for up in c.to_uppercase() {
                out.push(up);
            }
            word_start = false;
        } else {
            for lo in c.to_lowercase() {
                out.push(lo);
            }
        }
    }
    out
}

fn map_feed_source_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FeedSource> {
    Ok(FeedSource {
        id: r.get(0)?,
        base_url: r.get(1)?,
        last_checked_at: r.get(2)?,
        folder_id: r.get(3)?,
        folder_name: r.get(4)?,
        feed_count: r.get(5)?,
    })
}

const FEED_SOURCE_SELECT: &str = "
    SELECT fs.id, fs.base_url, fs.last_checked_at, fs.folder_id, fo.name,
           (SELECT COUNT(*) FROM feeds f WHERE f.feed_url LIKE fs.base_url || '%')
    FROM feed_sources fs
    LEFT JOIN folders fo ON fo.id = fs.folder_id
";

pub fn list_feed_sources(conn: &Connection) -> AppResult<Vec<FeedSource>> {
    let mut stmt = conn.prepare(&format!("{FEED_SOURCE_SELECT} ORDER BY fs.id"))?;
    let rows = stmt
        .query_map([], map_feed_source_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_feed_source(conn: &Connection, id: i64) -> AppResult<Option<FeedSource>> {
    Ok(conn
        .query_row(
            &format!("{FEED_SOURCE_SELECT} WHERE fs.id = ?1"),
            params![id],
            map_feed_source_row,
        )
        .optional()?)
}

/// Ensure the feed source has a linked folder (create + attach when missing).
/// Returns the folder id.
pub fn ensure_source_folder(conn: &Connection, source_id: i64, base_url: &str) -> AppResult<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT folder_id FROM feed_sources WHERE id = ?1",
            params![source_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(fid) = existing {
        // Folder may have been deleted (ON DELETE SET NULL); recreate if gone.
        let still: bool = conn
            .query_row(
                "SELECT 1 FROM folders WHERE id = ?1",
                params![fid],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if still {
            return Ok(fid);
        }
    }
    let name = folder_name_from_index_url(base_url);
    let folder_id = db::create_folder(conn, &name)?;
    conn.execute(
        "UPDATE feed_sources SET folder_id = ?2 WHERE id = ?1",
        params![source_id, folder_id],
    )?;
    Ok(folder_id)
}

pub fn insert_feed_source(conn: &Connection, base_url: &str) -> AppResult<i64> {
    if !valid_index_url(base_url) {
        return Err(AppError::code("invalidIndexUrl"));
    }
    let base = normalize_index_url(base_url);
    // Catch duplicates before create_folder so we never leave a stray folder
    // when the UNIQUE(base_url) insert would fail.
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM feed_sources WHERE base_url = ?1",
            params![base],
            |r| r.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Err(AppError::code("indexUrlExists"));
    }
    let folder_name = folder_name_from_index_url(&base);
    let folder_id = db::create_folder(conn, &folder_name)?;
    conn.execute(
        "INSERT INTO feed_sources(base_url, folder_id) VALUES (?1, ?2)",
        params![base, folder_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Idempotent insert (e.g. from `PAPR_FEED_SOURCE_URL` env).
pub fn seed_feed_source(conn: &Connection, env_url: &str) -> AppResult<Option<i64>> {
    if !valid_index_url(env_url) {
        return Ok(None);
    }
    let base = normalize_index_url(env_url);
    conn.execute(
        "INSERT OR IGNORE INTO feed_sources(base_url) VALUES (?1)",
        params![base],
    )?;
    let id = conn
        .query_row(
            "SELECT id FROM feed_sources WHERE base_url = ?1",
            params![base],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        let _ = ensure_source_folder(conn, id, &base)?;
    }
    Ok(id)
}

pub fn delete_feed_source(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM feed_sources WHERE id = ?1", params![id])?;
    Ok(())
}

fn touch_checked(conn: &Connection, source_id: i64, base_url: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE feed_sources SET base_url = ?2, last_checked_at = datetime('now') WHERE id = ?1",
        params![source_id, base_url],
    )?;
    Ok(())
}

fn feed_name_from_url(feed_url: &str) -> String {
    let path = Url::parse(feed_url)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_else(|| feed_url.to_string());
    let base = path.rsplit('/').next().unwrap_or(&path);
    let mut name = base
        .trim_end_matches(".xml")
        .trim_end_matches(".XML")
        .replace('-', " ")
        .replace('_', " ");
    name = name.trim().to_string();
    if name.is_empty() {
        return feed_url.to_string();
    }
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if first.is_ascii_lowercase() {
            return format!("{}{}", first.to_ascii_uppercase(), chars.as_str());
        }
    }
    name
}

fn list_stale(
    conn: &Connection,
    base_url: &str,
    discovered: &HashSet<String>,
) -> AppResult<Vec<StaleFeed>> {
    let pattern = format!("{base_url}%");
    let mut stmt = conn.prepare(
        "SELECT id, title, feed_url FROM feeds WHERE feed_url LIKE ?1 ORDER BY title, feed_url",
    )?;
    let rows = stmt.query_map(params![pattern], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut stale = Vec::new();
    for row in rows {
        let (id, name, url) = row?;
        if discovered.contains(&normalize_feed_url(&url)) {
            continue;
        }
        stale.push(StaleFeed {
            id,
            name,
            url,
            reason: "missing_from_index".into(),
        });
    }
    Ok(stale)
}

fn existing_feed_urls(conn: &Connection) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT feed_url FROM feeds")?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

async fn fetch_index(client: &reqwest::Client, index_url: &str) -> AppResult<String> {
    let resp = client.get(index_url).send().await?;
    if !resp.status().is_success() {
        return Err(AppError::other(format!("status {}", resp.status())));
    }
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        return Err(AppError::other("empty response"));
    }
    let limited = if bytes.len() > INDEX_FETCH_LIMIT {
        &bytes[..INDEX_FETCH_LIMIT]
    } else {
        &bytes
    };
    Ok(String::from_utf8_lossy(limited).into_owned())
}

/// Scan one index page: auto-add new XML feeds, report stale (never delete).
/// Never holds the DB lock across network I/O (rusqlite::Connection is !Send).
pub async fn scan(
    db: &Mutex<Connection>,
    client: &reqwest::Client,
    source_id: i64,
    base_url: &str,
) -> ScanResult {
    let base = normalize_index_url(base_url);
    let mut res = ScanResult {
        id: source_id,
        base_url: base.clone(),
        added: Vec::new(),
        skipped: 0,
        stale: Vec::new(),
        error: None,
    };
    if !valid_index_url(&base) {
        res.error = Some("invalid index URL (need http/https)".into());
        return res;
    }

    let body = match fetch_index(client, &base).await {
        Ok(b) => b,
        Err(e) => {
            res.error = Some(e.to_string());
            let conn = db.lock().await;
            let _ = touch_checked(&conn, source_id, &base);
            return res;
        }
    };

    let mut discovered = HashSet::new();
    for href in extract_xml_hrefs(&body) {
        if let Ok(full) = resolve_feed_href(&base, &href) {
            if !full.is_empty() {
                discovered.insert(full);
            }
        }
    }

    // DB work: list existing, insert new, list stale — no await while locked.
    let mut suspects: Vec<(i64, String, String)> = Vec::new();
    {
        let conn = db.lock().await;
        let folder_id = match ensure_source_folder(&conn, source_id, &base) {
            Ok(fid) => Some(fid),
            Err(e) => {
                log::warn!("feed source: ensure folder failed: {e}");
                None
            }
        };
        let existing = match existing_feed_urls(&conn) {
            Ok(m) => m,
            Err(e) => {
                res.error = Some(e.to_string());
                return res;
            }
        };
        let mut existing_norm: HashMap<String, bool> = existing
            .into_iter()
            .map(|u| (normalize_feed_url(&u), true))
            .collect();

        for feed_url in &discovered {
            if existing_norm.contains_key(feed_url) {
                res.skipped += 1;
                continue;
            }
            let name = feed_name_from_url(feed_url);
            match db::insert_feed(
                &conn,
                feed_url,
                None,
                &name,
                None,
                SourceType::Rss,
                folder_id,
            ) {
                Ok(id) => {
                    res.added.push(AddedFeed {
                        id,
                        name,
                        url: feed_url.clone(),
                    });
                    existing_norm.insert(feed_url.clone(), true);
                    log::info!("feed source: added feed {feed_url}");
                }
                Err(e) => {
                    log::warn!("feed source: insert failed for {feed_url}: {e}");
                    res.skipped += 1;
                    existing_norm.insert(feed_url.clone(), true);
                }
            }
        }

        // Adopt unfiled feeds already under this index prefix into the folder.
        if let Some(fid) = folder_id {
            let pattern = format!("{base}%");
            if let Err(e) = conn.execute(
                "UPDATE feeds SET folder_id = ?1
                 WHERE feed_url LIKE ?2 AND folder_id IS NULL",
                params![fid, pattern],
            ) {
                log::warn!("feed source: adopt into folder failed: {e}");
            }
        }

        match list_stale(&conn, &base, &discovered) {
            Ok(s) => res.stale = s,
            Err(e) => log::warn!("feed source: stale list failed: {e}"),
        }

        let exclude: HashSet<i64> = res.added.iter().map(|a| a.id).collect();
        let pattern = format!("{base}%");
        if let Ok(mut stmt) = conn.prepare(
            "SELECT f.id, f.title, f.feed_url FROM feeds f
             WHERE f.feed_url LIKE ?1
               AND (f.last_fetched_at IS NULL
                    OR NOT EXISTS (SELECT 1 FROM articles a WHERE a.feed_id = f.id))
             ORDER BY f.title, f.feed_url",
        ) {
            if let Ok(rows) = stmt.query_map(params![pattern], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            }) {
                for row in rows.flatten() {
                    let (id, name, url): (i64, String, String) = row;
                    if exclude.contains(&id) {
                        continue;
                    }
                    if !discovered.contains(&normalize_feed_url(&url)) {
                        continue;
                    }
                    suspects.push((id, name, url));
                }
            }
        }
        let _ = touch_checked(&conn, source_id, &base);
    }

    // Probe suspects outside the lock (serial — slow hosts).
    for (id, name, url) in suspects {
        match fetch::get(client, &url).await {
            Ok((bytes, _, _)) => {
                if let Ok(parsed) = parse::parse_feed(&bytes, &url) {
                    let title = parsed
                        .title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| name.clone());
                    let conn = db.lock().await;
                    let _ = db::update_feed_meta(&conn, id, Some(&title), None, None, None);
                    let rules = db::active_rules(&conn).unwrap_or_default();
                    let dedup = db::setting_flag(&conn, "dedup_enabled", true);
                    // First ingestion of a never-fetched feed is depth-capped
                    // (same `feed_initial_backfill` rule as the refresh loop):
                    // backfilling a whole history here would ingest hundreds of
                    // old items that each trigger an auto-tag LLM call.
                    let mut articles = parsed.articles;
                    let first = db::feed_article_count(&conn, id).unwrap_or(0) == 0;
                    let cap = db::setting_parsed::<i64>(&conn, "feed_initial_backfill", 50).max(0);
                    if first && cap > 0 {
                        db::cap_newest_articles(&mut articles, cap as usize);
                    }
                    for article in &articles {
                        let _ = db::upsert_article(&conn, id, article, dedup, &rules);
                    }
                    let _ = db::touch_feed(&conn, id);
                } else {
                    res.stale.push(StaleFeed {
                        id,
                        name,
                        url,
                        reason: "fetch_failed".into(),
                    });
                }
            }
            Err(e) => {
                log::info!("feed source: probe failed for {url}: {e}");
                res.stale.push(StaleFeed {
                    id,
                    name,
                    url,
                    reason: "fetch_failed".into(),
                });
            }
        }
    }

    res
}

/// Scan every configured feed source.
pub async fn sync_all(db: &Mutex<Connection>, client: &reqwest::Client) -> Vec<ScanResult> {
    let sources = {
        let conn = db.lock().await;
        match list_feed_sources(&conn) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("feed source: list failed: {e}");
                return Vec::new();
            }
        }
    };
    let mut results = Vec::with_capacity(sources.len());
    for s in sources {
        results.push(scan(db, client, s.id, &s.base_url).await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_index_url_cases() {
        assert_eq!(normalize_index_url(""), "");
        assert_eq!(
            normalize_index_url("  https://ex.com/ft  "),
            "https://ex.com/ft/"
        );
        assert_eq!(normalize_index_url("https://ex.com/ft/"), "https://ex.com/ft/");
        assert_eq!(
            normalize_index_url("https://ex.com/ft///"),
            "https://ex.com/ft/"
        );
    }

    #[test]
    fn valid_index_url_cases() {
        assert!(valid_index_url("https://ex.com/ft"));
        assert!(valid_index_url("http://localhost:8080/idx"));
        assert!(!valid_index_url(""));
        assert!(!valid_index_url("ftp://ex.com/x"));
        assert!(!valid_index_url("not a url"));
        assert!(!valid_index_url("/relative/path/"));
    }

    #[test]
    fn normalize_feed_url_cases() {
        assert_eq!(normalize_feed_url(""), "");
        assert_eq!(
            normalize_feed_url(" https://ex.com/a.xml "),
            "https://ex.com/a.xml"
        );
        assert_eq!(
            normalize_feed_url("https://ex.com/a.xml/"),
            "https://ex.com/a.xml"
        );
    }

    #[test]
    fn resolve_absolute_and_relative() {
        let base = "https://ex.com/ft/";
        assert_eq!(
            resolve_feed_href(base, "https://other.com/markets.xml").unwrap(),
            "https://other.com/markets.xml"
        );
        assert_eq!(
            resolve_feed_href(base, "asia.xml").unwrap(),
            "https://ex.com/ft/asia.xml"
        );
        assert_eq!(
            resolve_feed_href(base, "/root.xml").unwrap(),
            "https://ex.com/root.xml"
        );
        assert_eq!(
            resolve_feed_href("https://ex.com/ft", "asia.xml").unwrap(),
            "https://ex.com/ft/asia.xml"
        );
    }

    #[test]
    fn extract_xml_hrefs_dedupes() {
        let html = r#"
        <html><body>
          <a href="asia.xml">Asia</a>
          <a href='https://cdn.example/markets.xml'>Markets</a>
          <a href="asia.xml">dup</a>
          <a href="note.html">skip</a>
        </body></html>"#;
        let got = extract_xml_hrefs(html);
        assert_eq!(
            got,
            vec![
                "asia.xml".to_string(),
                "https://cdn.example/markets.xml".to_string()
            ]
        );
    }

    #[test]
    fn folder_name_from_index_url_cases() {
        assert_eq!(
            folder_name_from_index_url("https://bryan.yzcw.dpdns.org/foreignpolicy/"),
            "Foreignpolicy"
        );
        assert_eq!(
            folder_name_from_index_url("https://bryan.yzcw.dpdns.org/washingtonpost"),
            "Washingtonpost"
        );
        assert_eq!(
            folder_name_from_index_url("https://ex.com/ft/"),
            "Ft"
        );
        assert_eq!(
            folder_name_from_index_url("https://ex.com/wall-street/"),
            "Wall Street"
        );
        assert_eq!(
            folder_name_from_index_url("https://ex.com/"),
            "Ex.com"
        );
    }

    #[test]
    fn insert_feed_source_rejects_duplicate_base_url() {
        let path = std::env::temp_dir().join(format!(
            "papr-feed-source-dup-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let conn = crate::db::open(&path).unwrap();
        let url = "https://bryan.yzcw.dpdns.org/theinformation/";
        let id = insert_feed_source(&conn, url).unwrap();
        assert!(id > 0);
        // Trailing-slash variants normalize to the same key.
        let err = insert_feed_source(&conn, "https://bryan.yzcw.dpdns.org/theinformation")
            .unwrap_err();
        assert!(
            matches!(err, AppError::Coded("indexUrlExists")),
            "got {err:?}"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM feed_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
}
