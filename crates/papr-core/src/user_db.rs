//! Per-user article state (read / starred / read-later) on top of shared feeds.
//!
//! Shared content lives in `articles`; per-user marks live in
//! `user_article_states`. The legacy `articles.is_read` / `is_starred` /
//! `read_later` columns remain for the desktop/CLI single-user path.

use crate::error::AppResult;
use crate::models::*;
use rusqlite::{params, params_from_iter, types::Value, Connection};
use crate::db::{attach_article_tags, PREVIEW_SNIPPET_CHARS};

fn state_join(user_id: i64) -> (String, Value) {
    (
        "LEFT JOIN user_article_states uas ON uas.article_id = a.id AND uas.user_id = ?"
            .into(),
        Value::Integer(user_id),
    )
}

fn article_filter_for_user(
    query: &ArticleQuery,
    unread_only: bool,
) -> (Vec<String>, Vec<Value>) {
    let mut where_clauses: Vec<String> = vec!["1=1".into()];
    let mut binds: Vec<Value> = Vec::new();

    match query {
        ArticleQuery::All => {}
        ArticleQuery::Unread => {
            where_clauses.push("COALESCE(uas.is_read, 0) = 0".into());
        }
        ArticleQuery::Starred => {
            where_clauses.push("COALESCE(uas.is_starred, 0) = 1".into());
        }
        ArticleQuery::ReadLater => {
            where_clauses.push("COALESCE(uas.read_later, 0) = 1".into());
        }
        ArticleQuery::Feed(id) => {
            where_clauses.push("a.feed_id = ?".into());
            binds.push(Value::Integer(*id));
        }
        ArticleQuery::Folder(id) => {
            where_clauses.push("f.folder_id = ?".into());
            binds.push(Value::Integer(*id));
        }
        ArticleQuery::Tag(id) => {
            where_clauses.push(
                "a.id IN (SELECT article_id FROM article_tags WHERE tag_id = ?)".into(),
            );
            binds.push(Value::Integer(*id));
        }
    }
    if unread_only && !matches!(query, ArticleQuery::Unread) {
        where_clauses.push("COALESCE(uas.is_read, 0) = 0".into());
    }
    (where_clauses, binds)
}

fn article_order(oldest_first: bool, rank_first: bool) -> String {
    let date = if oldest_first {
        "datetime(COALESCE(a.published_at, a.fetched_at)) ASC, a.id ASC"
    } else {
        "datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC"
    };
    if rank_first {
        format!("fts.rank ASC, {date}")
    } else {
        date.to_string()
    }
}

pub fn list_feeds_for_user(conn: &Connection, user_id: i64) -> AppResult<Vec<Feed>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.feed_url, f.site_url, f.title, f.description, f.favicon_url,
                f.folder_id, f.source_type, f.last_fetched_at, f.fetch_error,
                (SELECT COUNT(*) FROM articles a
                   LEFT JOIN user_article_states uas
                     ON uas.article_id = a.id AND uas.user_id = ?1
                  WHERE a.feed_id = f.id AND COALESCE(uas.is_read, 0) = 0),
                f.refresh_interval_min, f.auto_translate, f.open_mode
         FROM feeds f ORDER BY f.title COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![user_id], |r| {
            Ok(Feed {
                id: r.get(0)?,
                feed_url: r.get(1)?,
                site_url: r.get(2)?,
                title: r.get(3)?,
                description: r.get(4)?,
                favicon_url: r.get(5)?,
                folder_id: r.get(6)?,
                source_type: r.get(7)?,
                last_fetched_at: r.get(8)?,
                fetch_error: r.get(9)?,
                unread_count: r.get(10)?,
                refresh_interval_min: r.get(11)?,
                auto_translate: r.get::<_, i64>(12)? != 0,
                open_mode: r.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Tags with per-user unread ("update") counts for the web multi-user path.
pub fn list_tags_for_user(
    conn: &Connection,
    user_id: i64,
    kind: Option<&str>,
) -> AppResult<Vec<Tag>> {
    let kind = match kind {
        Some(k) => Some(crate::db::normalize_tag_kind(k)?),
        None => None,
    };
    let sql = if kind.is_some() {
        "SELECT t.id, t.name, t.color, t.position, t.kind,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id),
                (SELECT COUNT(*) FROM article_tags at
                   JOIN articles a ON a.id = at.article_id
                   LEFT JOIN user_article_states uas
                     ON uas.article_id = a.id AND uas.user_id = ?1
                  WHERE at.tag_id = t.id AND COALESCE(uas.is_read, 0) = 0)
         FROM tags t WHERE t.kind = ?2
         ORDER BY t.position, t.name COLLATE NOCASE"
    } else {
        "SELECT t.id, t.name, t.color, t.position, t.kind,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id),
                (SELECT COUNT(*) FROM article_tags at
                   JOIN articles a ON a.id = at.article_id
                   LEFT JOIN user_article_states uas
                     ON uas.article_id = a.id AND uas.user_id = ?1
                  WHERE at.tag_id = t.id AND COALESCE(uas.is_read, 0) = 0)
         FROM tags t ORDER BY t.position, t.name COLLATE NOCASE"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(k) = kind {
        stmt.query_map(params![user_id, k], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                position: r.get(3)?,
                kind: r.get(4)?,
                article_count: r.get(5)?,
                unread_count: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map(params![user_id], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                position: r.get(3)?,
                kind: r.get(4)?,
                article_count: r.get(5)?,
                unread_count: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

pub fn list_articles_for_user(
    conn: &Connection,
    user_id: i64,
    query: &ArticleQuery,
    unread_only: bool,
    search: Option<&str>,
    oldest_first: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    list_articles_for_user_sorted(
        conn,
        user_id,
        query,
        unread_only,
        search,
        oldest_first,
        /* sort_by_relevance */ true,
        limit,
        offset,
    )
}

pub fn list_articles_for_user_sorted(
    conn: &Connection,
    user_id: i64,
    query: &ArticleQuery,
    unread_only: bool,
    search: Option<&str>,
    oldest_first: bool,
    sort_by_relevance: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    let (join_sql, join_bind) = state_join(user_id);
    let (mut where_clauses, mut binds) = article_filter_for_user(query, unread_only);

    let raw_search = search.map(|s| s.trim()).filter(|s| !s.is_empty());
    let compiled = raw_search.map(|s| {
        crate::wordcloud_dict::process_dict().with_dict(|dict| {
            crate::search::compile_search_with_dict(
                s,
                crate::search::SearchMode::Strict,
                Some(dict),
            )
        })
    });
    let match_nothing = raw_search.is_some()
        && compiled.as_ref().is_some_and(|c| c.is_empty());
    let has_fts = compiled
        .as_ref()
        .and_then(|c| c.match_expr.as_ref())
        .is_some();
    let has_feed = compiled
        .as_ref()
        .is_some_and(|c| !c.feed_prefixes.is_empty());

    let mut sql = format!(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author,
                substr(a.body_text,1,{snippet_len}), a.image_url, a.url, a.published_at,
                COALESCE(uas.is_read, 0), COALESCE(uas.is_starred, 0), COALESCE(uas.read_later, 0)
         FROM articles a JOIN feeds f ON f.id = a.feed_id {join_sql} ",
        snippet_len = PREVIEW_SNIPPET_CHARS,
        join_sql = join_sql,
    );
    // join bind must come first (user_id in JOIN).
    let mut all_binds = vec![join_bind];
    let rank_first = has_fts && sort_by_relevance;
    if has_fts {
        let expr = compiled.as_ref().unwrap().match_expr.clone().unwrap();
        sql.push_str("JOIN articles_fts fts ON fts.rowid = a.id ");
        where_clauses.push("articles_fts MATCH ?".into());
        binds.push(Value::Text(expr));
    }
    if has_feed {
        for name in &compiled.as_ref().unwrap().feed_prefixes {
            where_clauses.push("unicode_lower(f.title) LIKE ?".into());
            binds.push(Value::Text(format!("{}%", name.to_lowercase())));
        }
    }
    if match_nothing {
        where_clauses.push("1=0".into());
    }
    sql.push_str("WHERE ");
    sql.push_str(&where_clauses.join(" AND "));
    // Active search defaults to FTS relevance; browse stays chronological
    // (same as `db::list_articles_sorted`).
    sql.push_str(" ORDER BY ");
    sql.push_str(&article_order(oldest_first, rank_first));
    sql.push(' ');
    sql.push_str("LIMIT ? OFFSET ?");
    binds.push(Value::Integer(limit));
    binds.push(Value::Integer(offset));
    all_binds.extend(binds);

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt
        .query_map(params_from_iter(all_binds), |r| {
            Ok(ArticleSummary {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                source_type: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                snippet: r.get(6)?,
                image_url: r.get(7)?,
                url: r.get(8)?,
                published_at: r.get(9)?,
                is_read: r.get::<_, i64>(10)? != 0,
                is_starred: r.get::<_, i64>(11)? != 0,
                read_later: r.get::<_, i64>(12)? != 0,
                tags: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    attach_article_tags(conn, &mut rows)?;
    Ok(rows)
}

pub fn article_index_for_user(
    conn: &Connection,
    user_id: i64,
    query: &ArticleQuery,
    unread_only: bool,
    oldest_first: bool,
    article_id: i64,
) -> AppResult<Option<i64>> {
    let (join_sql, join_bind) = state_join(user_id);
    let (where_clauses, mut binds) = article_filter_for_user(query, unread_only);
    let sql = format!(
        "SELECT pos FROM (
             SELECT a.id AS aid,
                    ROW_NUMBER() OVER (ORDER BY {order}) - 1 AS pos
             FROM articles a JOIN feeds f ON f.id = a.feed_id {join_sql}
             WHERE {where_sql}
         ) WHERE aid = ?",
        order = article_order(oldest_first, false),
        join_sql = join_sql,
        where_sql = where_clauses.join(" AND "),
    );
    let mut all_binds = vec![join_bind];
    all_binds.append(&mut binds);
    all_binds.push(Value::Integer(article_id));
    let pos = conn
        .prepare(&sql)?
        .query_row(params_from_iter(all_binds), |r| r.get::<_, i64>(0))
        .optional()?;
    Ok(pos)
}

pub fn get_article_for_user(
    conn: &Connection,
    user_id: i64,
    id: i64,
) -> AppResult<ArticleDetail> {
    let mut detail = conn.query_row(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author, a.url,
                a.content_html, a.extracted_html, a.image_url, a.published_at,
                COALESCE(uas.is_read, 0), COALESCE(uas.is_starred, 0), COALESCE(uas.read_later, 0),
                a.ai_summary, a.translated_html, a.translated_lang
         FROM articles a
         JOIN feeds f ON f.id = a.feed_id
         LEFT JOIN user_article_states uas ON uas.article_id = a.id AND uas.user_id = ?2
         WHERE a.id = ?1",
        params![id, user_id],
        |r| {
            Ok(ArticleDetail {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                source_type: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                url: r.get(6)?,
                content_html: r.get(7)?,
                extracted_html: r.get(8)?,
                image_url: r.get(9)?,
                published_at: r.get(10)?,
                is_read: r.get::<_, i64>(11)? != 0,
                is_starred: r.get::<_, i64>(12)? != 0,
                read_later: r.get::<_, i64>(13)? != 0,
                ai_summary: r.get(14)?,
                translated_html: r.get(15)?,
                translated_lang: r.get(16)?,
                enclosures: Vec::new(),
                tags: Vec::new(),
            })
        },
    )?;
    let mut stmt =
        conn.prepare("SELECT url, mime_type, length FROM enclosures WHERE article_id = ?1")?;
    detail.enclosures = stmt
        .query_map(params![id], |r| {
            Ok(Enclosure {
                url: r.get(0)?,
                mime_type: r.get(1)?,
                length: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    detail.tags = crate::db::tags_for_article(conn, id)?;
    Ok(detail)
}

fn upsert_flag(
    conn: &Connection,
    user_id: i64,
    article_id: i64,
    column: &str,
    value: bool,
) -> AppResult<()> {
    // Ensure a row exists, then set the one flag.
    conn.execute(
        "INSERT INTO user_article_states(user_id, article_id, is_read, is_starred, read_later)
         VALUES (?1, ?2, 0, 0, 0)
         ON CONFLICT(user_id, article_id) DO NOTHING",
        params![user_id, article_id],
    )?;
    let sql = format!(
        "UPDATE user_article_states SET {column} = ?3 WHERE user_id = ?1 AND article_id = ?2"
    );
    conn.execute(&sql, params![user_id, article_id, value as i64])?;
    Ok(())
}

pub fn set_read_for_user(
    conn: &Connection,
    user_id: i64,
    article_id: i64,
    read: bool,
) -> AppResult<()> {
    upsert_flag(conn, user_id, article_id, "is_read", read)
}

pub fn set_starred_for_user(
    conn: &Connection,
    user_id: i64,
    article_id: i64,
    starred: bool,
) -> AppResult<()> {
    upsert_flag(conn, user_id, article_id, "is_starred", starred)
}

pub fn set_read_later_for_user(
    conn: &Connection,
    user_id: i64,
    article_id: i64,
    value: bool,
) -> AppResult<()> {
    upsert_flag(conn, user_id, article_id, "read_later", value)
}

pub fn mark_all_read_for_user(
    conn: &Connection,
    user_id: i64,
    query: &ArticleQuery,
) -> AppResult<usize> {
    let (pred, id): (&str, Option<i64>) = match query {
        ArticleQuery::All | ArticleQuery::Unread => ("1", None),
        ArticleQuery::Starred => ("COALESCE(uas.is_starred, 0) = 1", None),
        ArticleQuery::ReadLater => ("COALESCE(uas.read_later, 0) = 1", None),
        ArticleQuery::Feed(id) => ("a.feed_id = ?1", Some(*id)),
        ArticleQuery::Folder(id) => (
            "a.feed_id IN (SELECT id FROM feeds WHERE folder_id = ?1)",
            Some(*id),
        ),
        ArticleQuery::Tag(id) => (
            "a.id IN (SELECT article_id FROM article_tags WHERE tag_id = ?1)",
            Some(*id),
        ),
    };

    let tx = conn.unchecked_transaction()?;
    // Upsert is_read=1 for matching articles. ON CONFLICT only flips is_read so
    // existing star / read-later flags on the row are preserved.
    let n = if let Some(scope_id) = id {
        let pred = pred.replace("?1", "?2");
        let sql = format!(
            "INSERT INTO user_article_states(user_id, article_id, is_read, is_starred, read_later)
             SELECT ?1, a.id, 1, 0, 0
             FROM articles a
             LEFT JOIN user_article_states uas ON uas.article_id = a.id AND uas.user_id = ?1
             WHERE {pred} AND COALESCE(uas.is_read, 0) = 0
             ON CONFLICT(user_id, article_id) DO UPDATE SET is_read = 1"
        );
        tx.execute(&sql, params![user_id, scope_id])?
    } else {
        let sql = format!(
            "INSERT INTO user_article_states(user_id, article_id, is_read, is_starred, read_later)
             SELECT ?1, a.id, 1, 0, 0
             FROM articles a
             LEFT JOIN user_article_states uas ON uas.article_id = a.id AND uas.user_id = ?1
             WHERE {pred} AND COALESCE(uas.is_read, 0) = 0
             ON CONFLICT(user_id, article_id) DO UPDATE SET is_read = 1"
        );
        tx.execute(&sql, params![user_id])?
    };
    tx.commit()?;
    Ok(n)
}

pub fn smart_counts_for_user(conn: &Connection, user_id: i64) -> AppResult<(i64, i64, i64)> {
    Ok(conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM articles a
               LEFT JOIN user_article_states uas ON uas.article_id = a.id AND uas.user_id = ?1
              WHERE COALESCE(uas.is_read, 0) = 0),
            (SELECT COUNT(*) FROM user_article_states WHERE user_id = ?1 AND is_starred = 1),
            (SELECT COUNT(*) FROM user_article_states WHERE user_id = ?1 AND read_later = 1)",
        params![user_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

// Need OptionalExtension for article_index_for_user
use rusqlite::OptionalExtension;
