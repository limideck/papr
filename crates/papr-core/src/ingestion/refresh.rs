//! Headless feed refresh — the UI-free core of a refresh cycle.
//!
//! Selects the sources to touch, fetches them with bounded concurrency, ingests
//! new articles, polls newsletter mailboxes over IMAP, and runs retention
//! cleanup. Progress is reported through an `on_event` callback so callers can
//! render it however they like.
//!
//! The desktop app wraps [`refresh_core`] with a Tauri progress channel,
//! notifications, FreshRSS sync and tray updates (see `papr_lib::scheduler`);
//! the agent CLI drives it directly, forwarding events to stderr.

use crate::db;
use crate::error::AppResult;
use crate::ingestion::newsletter;
use crate::ingestion::{fetch, parse};
use crate::models::{RefreshProgress, SourceType};
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

/// Wall-clock cap for polling one newsletter mailbox over IMAP. Generous enough
/// for a slow mailbox with large messages, short enough that a wedged server
/// never stalls the refresh cycle. Shared with the interactive
/// `add_newsletter_source` probe so a hung server can't hang the Add dialog.
pub const NEWSLETTER_POLL_TIMEOUT_SECS: u64 = 90;

/// Result of polling one newsletter mailbox: the feed id paired with either the
/// raw RFC822 message bytes or an error string describing the failure.
type MailboxPoll = (i64, Result<Vec<Vec<u8>>, String>);

/// Outcome of a [`refresh_core`] run.
#[derive(Clone, Copy, Debug)]
pub struct RefreshSummary {
    /// Number of newly inserted articles across all sources.
    pub new_articles: usize,
    /// `false` only when a `Due`-scoped run found nothing due and skipped the
    /// pipeline entirely. Lets the desktop scheduler keep idle ticks genuinely
    /// idle (no notifications / sync / tray refresh on an empty cycle).
    pub ran: bool,
}

/// Which feeds a refresh run should touch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RefreshScope {
    /// Every feed and newsletter — the manual refresh and OPML import.
    All,
    /// Only sources whose per-feed (or global) interval has elapsed — the
    /// background scheduler. An empty due-set skips the whole pipeline.
    Due,
    /// A single feed by id (and its newsletter mailbox, if any) — the per-feed
    /// manual refresh (`refresh --feed <id>`). Always runs the pipeline.
    Feed(i64),
    /// Every feed in one folder by id — the per-folder manual refresh. Always
    /// runs the pipeline.
    Folder(i64),
}

/// Insert a batch of articles for one feed in bounded chunks, releasing the
/// shared DB lock between each so concurrent queries aren't starved while a
/// large feed (hundreds of items) is being ingested. Returns the count newly
/// inserted; `label` only distinguishes the warning text (`rss`/`newsletter`).
async fn upsert_articles(
    db: &Mutex<Connection>,
    feed_id: i64,
    articles: &[db::NewArticle],
    dedup: bool,
    rules: &[crate::models::Rule],
    label: &str,
) -> usize {
    let mut new_count = 0usize;
    for chunk in articles.chunks(64) {
        let conn = db.lock().await;
        for article in chunk {
            match db::upsert_article(&conn, feed_id, article, dedup, rules) {
                Ok(true) => new_count += 1,
                Ok(false) => {}
                Err(e) => log::warn!("{label} upsert failed (feed {feed_id}): {e}"),
            }
        }
    }
    new_count
}

/// Outcome of fetching one feed.
enum Outcome {
    NotModified,
    Updated {
        parsed: parse::ParsedFeed,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    Failed(String),
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    etag: Option<String>,
    last_modified: Option<String>,
) -> Outcome {
    match fetch::conditional_get(client, url, etag.as_deref(), last_modified.as_deref()).await {
        Ok(fetch::Fetched::NotModified) => Outcome::NotModified,
        Ok(fetch::Fetched::Body {
            bytes,
            etag,
            last_modified,
        }) => match parse::parse_feed(&bytes, url) {
            Ok(parsed) => Outcome::Updated {
                parsed,
                etag,
                last_modified,
            },
            Err(e) => Outcome::Failed(e.to_string()),
        },
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Refresh the sources selected by `scope`: fetch with bounded concurrency,
/// ingest new articles, poll newsletters, and run retention cleanup. Reports
/// progress through `on_event` and returns the new-article count.
///
/// UI-free and side-effect-light: it performs no cross-process locking (callers
/// serialize), no desktop notifications and no FreshRSS sync. `db` is the
/// writer connection behind an async mutex; `client` is a shared HTTP client
/// (cheap to clone per feed).
pub async fn refresh_core(
    db: &Mutex<Connection>,
    client: &reqwest::Client,
    scope: RefreshScope,
    mut on_event: impl FnMut(RefreshProgress),
) -> AppResult<RefreshSummary> {
    let (feeds, newsletters, concurrency, dedup, rules) = {
        let conn = db.lock().await;
        // The global default interval for feeds without a per-feed override.
        let global_min = db::get_setting(&conn, "refresh_interval_min")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|m| *m >= 5)
            .map(|m| m.min(db::REFRESH_OFF_MINUTES))
            .unwrap_or(30);
        let (feeds, newsletters) = match scope {
            RefreshScope::All => (
                db::feeds_to_refresh(&conn)?,
                db::newsletter_sources_to_poll(&conn).unwrap_or_default(),
            ),
            RefreshScope::Due => (
                db::feeds_due_for_refresh(&conn, global_min)?,
                db::newsletter_sources_due_to_poll(&conn, global_min).unwrap_or_default(),
            ),
            // The HTTP fetch set excludes newsletter sources (`source_type !=
            // 'newsletter'` in the query), so a newsletter feed's synthetic
            // `newsletter://` URL is never handed to `fetch_one`; it is polled
            // over IMAP via the separate newsletter set instead.
            RefreshScope::Feed(id) => (
                db::feeds_to_refresh_for_feed(&conn, id)?,
                db::newsletter_sources_for_feed(&conn, id).unwrap_or_default(),
            ),
            RefreshScope::Folder(id) => (
                db::feeds_to_refresh_in_folder(&conn, id)?,
                db::newsletter_sources_in_folder(&conn, id).unwrap_or_default(),
            ),
        };
        let concurrency =
            db::setting_parsed::<i64>(&conn, "net_concurrency", 6).clamp(1, 16) as usize;
        let dedup = db::setting_flag(&conn, "dedup_enabled", true);
        let rules = db::active_rules(&conn).unwrap_or_default();
        (feeds, newsletters, concurrency, dedup, rules)
    };

    // Nothing due this cycle: emit a no-op Started/Finished and bow out before
    // the heavier tail. The manual refresh (scope All) always runs the pipeline.
    if scope == RefreshScope::Due && feeds.is_empty() && newsletters.is_empty() {
        on_event(RefreshProgress::Started { total: 0 });
        on_event(RefreshProgress::Finished { new_articles: 0 });
        return Ok(RefreshSummary {
            new_articles: 0,
            ran: false,
        });
    }

    on_event(RefreshProgress::Started { total: feeds.len() });

    let sem = Arc::new(Semaphore::new(concurrency));
    // The feed URL travels back out alongside the outcome — `refine_source_type`
    // needs it for the Mastodon `/@user.rss` pattern check below.
    let mut set: JoinSet<(i64, String, Outcome)> = JoinSet::new();
    for (id, url, etag, last_modified) in feeds {
        let client = client.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await;
            let outcome = fetch_one(&client, &url, etag, last_modified).await;
            (id, url, outcome)
        });
    }

    let mut total_new = 0usize;
    while let Some(joined) = set.join_next().await {
        let Ok((feed_id, feed_url, outcome)) = joined else {
            continue;
        };
        let mut new_here = 0usize;
        let mut error: Option<String> = None;

        match outcome {
            Outcome::NotModified => {
                let conn = db.lock().await;
                let _ = db::touch_feed(&conn, feed_id);
            }
            Outcome::Failed(e) => {
                let conn = db.lock().await;
                let _ = db::set_feed_error(&conn, feed_id, &e);
                error = Some(e);
            }
            Outcome::Updated {
                parsed,
                etag,
                last_modified,
            } => {
                new_here +=
                    upsert_articles(db, feed_id, &parsed.articles, dedup, &rules, "rss").await;
                let conn = db.lock().await;
                let _ = db::update_feed_meta(
                    &conn,
                    feed_id,
                    parsed.title.as_deref(),
                    parsed.site_url.as_deref(),
                    parsed.description.as_deref(),
                    parsed.icon.as_deref(),
                );
                let _ = db::set_feed_fetch_state(
                    &conn,
                    feed_id,
                    etag.as_deref(),
                    last_modified.as_deref(),
                    None,
                );
                // Promote a still-generic `'rss'` feed to its real kind now that
                // the parsed document reveals it (audio enclosures → podcast,
                // `/@user.rss` → mastodon). A no-op for an already classified feed.
                let refined = parse::refine_source_type(SourceType::Rss, &parsed, &feed_url);
                let _ = db::refine_feed_source_type(&conn, feed_id, refined);
            }
        }

        total_new += new_here;
        on_event(RefreshProgress::FeedDone {
            feed_id,
            new_articles: new_here,
            error,
        });
    }

    // Newsletter sources: poll each configured IMAP mailbox and ingest any new
    // messages as articles, alongside the RSS refresh above.
    total_new += poll_newsletters(db, newsletters, dedup, &rules).await;

    // Retention: drop old read articles when a finite window is configured. The
    // DELETE scans the whole table, so throttle it to once per day rather than
    // running on every refresh cycle.
    {
        let conn = db.lock().await;
        let retention = db::get_setting(&conn, "retention_days").ok().flatten();
        if let Some(days) = retention.and_then(|v| v.parse::<i64>().ok()) {
            let now = chrono::Utc::now().timestamp();
            let last_run = db::setting_parsed::<i64>(&conn, "retention_last_run", 0);
            if now - last_run >= 86_400 {
                match db::cleanup_old_articles(&conn, days) {
                    Ok(removed) => {
                        if removed > 0 {
                            log::info!("retention: removed {removed} old articles");
                        }
                        let _ = db::set_setting(&conn, "retention_last_run", &now.to_string());
                    }
                    Err(e) => log::warn!("retention cleanup failed: {e}"),
                }
            }
        }
    }

    on_event(RefreshProgress::Finished {
        new_articles: total_new,
    });
    Ok(RefreshSummary {
        new_articles: total_new,
        ran: true,
    })
}

/// Poll every configured email-newsletter source over IMAP and ingest any new
/// messages as articles. Runs as part of [`refresh_core`] so newsletters
/// refresh on the same cadence as RSS feeds. Returns the new-article count.
///
/// A failure for one mailbox (bad credentials, server down) is recorded as the
/// feed's `fetch_error` and does not abort the others.
async fn poll_newsletters(
    db: &Mutex<Connection>,
    sources: Vec<(i64, newsletter::NewsletterConfig)>,
    dedup: bool,
    rules: &[crate::models::Rule],
) -> usize {
    if sources.is_empty() {
        return 0;
    }

    // Poll the mailboxes concurrently — each IMAP fetch is slow and fully
    // independent. Bounded by a semaphore so a user with many newsletters does
    // not open dozens of TLS connections at once.
    let sem = Arc::new(Semaphore::new(4));
    let mut set: JoinSet<MailboxPoll> = JoinSet::new();
    for (feed_id, cfg) in sources {
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await;
            // `imap` is a blocking crate with no per-operation timeout: a server
            // that completes the handshake but stalls mid-command would block
            // forever. Bound the whole fetch with a wall-clock timeout so a hung
            // mailbox degrades to a per-feed error instead of wedging the run.
            //
            // Caveat: `timeout` only abandons the `JoinHandle`; the
            // `spawn_blocking` thread keeps running until its socket read
            // unblocks. A truly cancellable poll needs a socket read-timeout,
            // but the pinned `imap` alpha exposes no public accessor for the
            // session's stream (its `SetReadTimeout` is crate-internal), so that
            // would mean hand-rolling the rustls handshake. Tracked as a
            // follow-up; acceptable here because a permanently-stalling mailbox
            // is a rare, user-fixable misconfiguration.
            let fetched = match tokio::time::timeout(
                Duration::from_secs(NEWSLETTER_POLL_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || newsletter::fetch_recent(&cfg, 50)),
            )
            .await
            {
                Ok(joined) => joined
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string())),
                Err(_) => Err(format!(
                    "IMAP poll timed out after {NEWSLETTER_POLL_TIMEOUT_SECS}s"
                )),
            };
            (feed_id, fetched)
        });
    }

    let mut total_new = 0usize;
    while let Some(joined) = set.join_next().await {
        let Ok((feed_id, fetched)) = joined else {
            continue;
        };
        match fetched {
            Ok(messages) => {
                // Parse the RFC822 bytes into articles *before* taking the DB
                // lock — `email_to_article` sanitizes HTML and is CPU-bound, so
                // doing it inside the locked scope would starve concurrent queries.
                let articles: Vec<_> = messages
                    .iter()
                    .filter_map(|raw| newsletter::email_to_article(raw))
                    .map(|p| p.article)
                    .collect();
                total_new +=
                    upsert_articles(db, feed_id, &articles, dedup, rules, "newsletter").await;
                let conn = db.lock().await;
                let _ = db::touch_feed(&conn, feed_id);
            }
            Err(e) => {
                log::warn!("newsletter poll failed (feed {feed_id}): {e}");
                let conn = db.lock().await;
                let _ = db::set_feed_error(&conn, feed_id, &e);
            }
        }
    }
    total_new
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceType;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Minimal RSS 2.0 document with two items.
    const FEED_BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <link>http://example.com/</link>
    <description>test</description>
    <item><title>First Story</title><guid>1</guid><link>http://example.com/1</link><description>one</description></item>
    <item><title>Second Story</title><guid>2</guid><link>http://example.com/2</link><description>two</description></item>
  </channel>
</rss>"#;

    /// An item whose title contains *overlapping* CJK aliases (美国国防部 is a
    /// longer alias that occupies the span 美国 also starts at). Regression for
    /// the wordcloud `match_entities` UTF-8 byte-offset panic that killed the
    /// background refresh loop mid-ingestion.
    const CJK_FEED_BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <link>http://example.com/</link>
    <description>test</description>
    <item><title>美国国防部 invests heavily</title><guid>cjk1</guid><link>http://example.com/cjk1</link><description>Pentagon spending news</description></item>
  </channel>
</rss>"#;

    /// Serve `body` for the next `requests` HTTP requests on an ephemeral
    /// loopback port and return the base URL. Each response is a plain 200
    /// with the body; used to exercise the real fetch → parse → ingest path
    /// without external network access.
    async fn serve_feed(body: &'static str, requests: u32) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..requests {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/feed.xml")
    }

    /// A migrated writer connection on a unique temp file, wrapped in the same
    /// async mutex shape `refresh_core` expects.
    fn test_state() -> (PathBuf, Arc<Mutex<Connection>>) {
        let path = std::env::temp_dir().join(format!(
            "papr-refresh-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let conn = db::open(&path).unwrap();
        (path, Arc::new(Mutex::new(conn)))
    }

    #[tokio::test]
    async fn due_scope_with_nothing_due_returns_ran_false() {
        let (_path, db) = test_state();
        // No feeds at all → the Due pipeline bows out without fetching.
        let client = fetch::build_client(10, "none");
        let summary = refresh_core(&db, &client, RefreshScope::Due, |_| {})
            .await
            .unwrap();
        assert!(!summary.ran);
        assert_eq!(summary.new_articles, 0);
    }

    #[tokio::test]
    async fn refresh_core_ingests_feed_over_http() {
        let url = serve_feed(FEED_BODY, 1).await;
        let (_path, db) = test_state();
        {
            let conn = db.lock().await;
            db::insert_feed(
                &conn,
                &url,
                None,
                "Test Feed",
                None,
                SourceType::Rss,
                None,
            )
            .unwrap();
        }
        let client = fetch::build_client(10, "none");
        let summary = refresh_core(&db, &client, RefreshScope::All, |_| {})
            .await
            .unwrap();
        assert!(summary.ran);
        assert_eq!(summary.new_articles, 2, "both items must be ingested");

        let conn = db.lock().await;
        let articles: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(articles, 2);
        let (fetched, err): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT last_fetched_at, fetch_error FROM feeds WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(fetched.is_some(), "feed must be touched after a fetch");
        assert!(err.is_none(), "no fetch error expected, got {err:?}");
    }

    #[tokio::test]
    async fn refresh_ingests_cjk_overlapping_alias_article_without_panic() {
        // Regression for the production outage: ingesting an article whose
        // title contains overlapping CJK aliases used to panic inside
        // wordcloud `match_entities` (byte-offset advance mid-char), which
        // unwound and killed the whole refresh task. With the process dict
        // loaded (present on dev/prod machines), this panicked before the fix.
        let url = serve_feed(CJK_FEED_BODY, 1).await;
        let (_path, db) = test_state();
        {
            let conn = db.lock().await;
            db::insert_feed(
                &conn,
                &url,
                None,
                "Test Feed",
                None,
                SourceType::Rss,
                None,
            )
            .unwrap();
        }
        let client = fetch::build_client(10, "none");
        let summary = refresh_core(&db, &client, RefreshScope::All, |_| {})
            .await
            .unwrap();
        assert!(summary.ran);
        assert_eq!(summary.new_articles, 1, "CJK item must be ingested");

        let conn = db.lock().await;
        let title: String = conn
            .query_row("SELECT title FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "美国国防部 invests heavily");
    }
}
