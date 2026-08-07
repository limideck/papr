//! Background feed refresh, feed-source index sync, auto-tag, and wordcloud backfill.

use crate::state::AppState;
use papr_core::auto_tag::{self, MAX_ATTEMPTS};
use papr_core::db;
use papr_core::error::AppError;
use papr_core::ingestion::feed_source;
use papr_core::ingestion::refresh::{self, RefreshScope};
use papr_core::wordcloud;
use std::time::Duration;

/// Default interval between due-feed refresh ticks (seconds).
const REFRESH_TICK_SECS: u64 = 60;
/// Feed-source index sync interval (6 hours, matching FO behaviour).
const FEED_SOURCE_TICK_SECS: u64 = 6 * 60 * 60;
/// Auto-tag worker poll interval when idle (queue empty or tagging disabled).
const AUTO_TAG_IDLE_SECS: u64 = 5;
/// Reclaim `processing` rows older than this (crashed worker).
const AUTO_TAG_STALE_MINUTES: i64 = 15;
/// Default concurrent auto-tag workers. Override with `PAPR_AUTO_TAG_CONCURRENCY`.
const DEFAULT_AUTO_TAG_CONCURRENCY: usize = 3;
/// Word-cloud term backfill poll interval when draining a backlog.
const WORDCLOUD_BACKFILL_TICK_SECS: u64 = 2;

fn auto_tag_concurrency() -> usize {
    std::env::var("PAPR_AUTO_TAG_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_AUTO_TAG_CONCURRENCY)
        .clamp(1, 16)
}

pub fn spawn_background_jobs(state: AppState) {
    tokio::spawn(refresh_loop(state.clone()));
    tokio::spawn(feed_source_loop(state.clone()));
    let n = auto_tag_concurrency();
    tracing::info!(workers = n, "starting auto-tag workers");
    for worker_id in 0..n {
        tokio::spawn(auto_tag_worker(state.clone(), worker_id));
    }
    tokio::spawn(wordcloud_backfill_loop(state));
}

async fn refresh_loop(state: AppState) {
    // Initial delay so startup (admin seed, first request) isn't contended.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut ticker = tokio::time::interval(Duration::from_secs(REFRESH_TICK_SECS));
    loop {
        ticker.tick().await;
        let db = state.db.clone();
        let client = state.http.clone();
        match refresh::refresh_core(&db, &client, RefreshScope::Due, |_| {}).await {
            Ok(summary) if summary.ran => {
                tracing::info!(
                    new_articles = summary.new_articles,
                    "background refresh complete"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("background refresh failed: {e}"),
        }
        // Optional retention cleanup once per tick when configured.
        let days = {
            let conn = state.db.lock().await;
            db::get_setting(&conn, "retention_days")
                .ok()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|d| *d > 0)
        };
        if let Some(days) = days {
            let conn = state.db.lock().await;
            if let Err(e) = db::cleanup_old_articles(&conn, days) {
                tracing::warn!("retention cleanup failed: {e}");
            }
        }
    }
}

async fn feed_source_loop(state: AppState) {
    // Run once shortly after boot, then every 6 hours.
    tokio::time::sleep(Duration::from_secs(15)).await;
    loop {
        let results = feed_source::sync_all(&state.db, &state.http).await;
        let added: usize = results.iter().map(|r| r.added.len()).sum();
        if added > 0 || results.iter().any(|r| r.error.is_some()) {
            tracing::info!(
                sources = results.len(),
                added,
                "feed source sync complete"
            );
        }
        tokio::time::sleep(Duration::from_secs(FEED_SOURCE_TICK_SECS)).await;
    }
}

/// One concurrent auto-tag worker.
///
/// Continuous drain: after finishing a job (ok or hard fail), claim the next
/// immediately. Sleep only when the queue is empty or tagging is disabled.
async fn auto_tag_worker(state: AppState, worker_id: usize) {
    // Stagger startup so workers don't all contend on the first claim.
    tokio::time::sleep(Duration::from_secs(10 + worker_id as u64)).await;
    if worker_id == 0 {
        let conn = state.db.lock().await;
        match db::reclaim_stale_auto_tag_jobs(&conn, Some(AUTO_TAG_STALE_MINUTES)) {
            Ok(n) if n > 0 => tracing::info!(reclaimed = n, "reclaimed stale auto-tag jobs"),
            Ok(_) => {}
            Err(e) => tracing::warn!("auto-tag reclaim failed: {e}"),
        }
    }

    loop {
        let enabled = {
            let conn = state.db.lock().await;
            db::setting_flag(&conn, "auto_tag_enabled", false)
                || db::setting_flag(&conn, "ai_tag_enabled", false)
        };
        if !enabled {
            tokio::time::sleep(Duration::from_secs(AUTO_TAG_IDLE_SECS)).await;
            continue;
        }

        // Manual sync auto-tag (reader click) holds the LLM / writer mutex —
        // skip claiming so backlog workers do not pile on in parallel.
        if state.manual_auto_tag_busy() {
            tokio::time::sleep(Duration::from_millis(400)).await;
            continue;
        }

        let job = {
            let conn = state.db.lock().await;
            // Cheap periodic reclaim so a crashed peer does not leave jobs stuck.
            if worker_id == 0 {
                let _ = db::reclaim_stale_auto_tag_jobs(&conn, Some(AUTO_TAG_STALE_MINUTES));
            }
            // Re-check under the lock window: a manual request may have started
            // between the busy probe and acquiring the DB mutex.
            if state.manual_auto_tag_busy() {
                None
            } else {
                match db::claim_auto_tag_job(&conn) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::warn!(worker_id, "auto-tag claim failed: {e}");
                        None
                    }
                }
            }
        };
        let Some((article_id, _attempts)) = job else {
            // Brief nap when a race lost to manual busy; otherwise idle poll.
            let wait = if state.manual_auto_tag_busy() {
                Duration::from_millis(400)
            } else {
                Duration::from_secs(AUTO_TAG_IDLE_SECS)
            };
            tokio::time::sleep(wait).await;
            continue;
        };

        match auto_tag::process_article(&state.db, &state.http, article_id).await {
            Ok(()) => {
                let conn = state.db.lock().await;
                if let Err(e) = db::mark_auto_tag_done(&conn, article_id) {
                    tracing::warn!(article_id, "auto-tag mark done failed: {e}");
                } else {
                    tracing::debug!(worker_id, article_id, "auto-tag complete");
                }
                // Backlog remains → loop immediately (no idle sleep).
            }
            Err(e) => {
                // Disabled mid-flight: release claim; do not burn attempts.
                if matches!(&e, AppError::Coded("autoTagDisabled")) {
                    let conn = state.db.lock().await;
                    if let Err(rel) = db::release_auto_tag_job(&conn, article_id) {
                        tracing::warn!(article_id, "auto-tag release failed: {rel}");
                    }
                    tokio::time::sleep(Duration::from_secs(AUTO_TAG_IDLE_SECS)).await;
                    continue;
                }
                let detail = e.to_string();
                tracing::warn!(worker_id, article_id, error = %detail, "auto-tag failed");
                let conn = state.db.lock().await;
                if let Err(mark_err) =
                    db::mark_auto_tag_failure(&conn, article_id, &detail, MAX_ATTEMPTS)
                {
                    tracing::warn!(article_id, "auto-tag mark failure failed: {mark_err}");
                }
                // Keep draining after hard failures.
            }
        }
    }
}

/// Drain articles missing/stale word-cloud terms. Tokenization runs outside
/// the global DB mutex; only fetch + write hold the lock briefly.
async fn wordcloud_backfill_loop(state: AppState) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    let mut ticker = tokio::time::interval(Duration::from_secs(WORDCLOUD_BACKFILL_TICK_SECS));
    loop {
        ticker.tick().await;

        let (dict_version, rows) = {
            let conn = state.db.lock().await;
            match wordcloud::fetch_backfill_batch(&conn, wordcloud::BACKFILL_BATCH) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("wordcloud backfill fetch failed: {e}");
                    continue;
                }
            }
        };
        if rows.is_empty() {
            // Idle longer when caught up.
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        }

        let prepared = state
            .wordcloud
            .with_dict(|dict| wordcloud::tokenize_backfill_batch(&rows, dict));

        let conn = state.db.lock().await;
        match wordcloud::write_backfill_batch(&conn, dict_version, &prepared) {
            Ok(()) => {
                tracing::info!(
                    processed = prepared.len(),
                    "wordcloud term backfill batch complete"
                );
            }
            Err(e) => tracing::warn!("wordcloud backfill write failed: {e}"),
        }
    }
}
