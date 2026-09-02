//! SQLite data layer. One file holds feeds, articles, FTS5 index and settings.
//! All SQL lives here; commands call typed functions, never raw SQL.

use crate::error::{AppError, AppResult};
use crate::models::*;
use rusqlite::functions::FunctionFlags;
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::Serialize;
use std::path::Path;
use std::sync::LazyLock;

/// Append-only schema migrations. Never edit a shipped migration — add a new one.
/// How many leading characters of an article's `body_text` form the list
/// "snippet". This single source of truth is shared by the list query (which
/// derives `ArticleSummary.snippet`) and the preview-translation command (which
/// translates the same slice), so the translated snippet always corresponds to
/// the original the list would otherwise show.
pub const PREVIEW_SNIPPET_CHARS: usize = 280;

static MIGRATIONS: LazyLock<Migrations> = LazyLock::new(|| {
    Migrations::new(vec![M::up(
        r#"
        CREATE TABLE folders (
            id        INTEGER PRIMARY KEY,
            name      TEXT NOT NULL,
            position  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE feeds (
            id              INTEGER PRIMARY KEY,
            feed_url        TEXT NOT NULL UNIQUE,
            site_url        TEXT,
            title           TEXT NOT NULL,
            description     TEXT,
            favicon_url     TEXT,
            folder_id       INTEGER REFERENCES folders(id) ON DELETE SET NULL,
            source_type     TEXT NOT NULL DEFAULT 'rss',
            etag            TEXT,
            last_modified   TEXT,
            last_fetched_at TEXT,
            fetch_error     TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE articles (
            id            INTEGER PRIMARY KEY,
            feed_id       INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
            guid          TEXT NOT NULL,
            url           TEXT,
            title         TEXT NOT NULL,
            author        TEXT,
            summary       TEXT,
            content_html  TEXT,
            extracted_html TEXT,
            body_text     TEXT NOT NULL DEFAULT '',
            image_url     TEXT,
            ai_summary    TEXT,
            published_at  TEXT,
            fetched_at    TEXT NOT NULL DEFAULT (datetime('now')),
            is_read       INTEGER NOT NULL DEFAULT 0,
            is_starred    INTEGER NOT NULL DEFAULT 0,
            read_later    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(feed_id, guid)
        );

        CREATE INDEX idx_articles_feed      ON articles(feed_id);
        CREATE INDEX idx_articles_published ON articles(published_at DESC);
        CREATE INDEX idx_articles_unread    ON articles(is_read) WHERE is_read = 0;

        CREATE TABLE enclosures (
            id         INTEGER PRIMARY KEY,
            article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
            url        TEXT NOT NULL,
            mime_type  TEXT,
            length     INTEGER
        );
        CREATE INDEX idx_enclosures_article ON enclosures(article_id);

        CREATE VIRTUAL TABLE articles_fts USING fts5(
            title, body, tokenize = 'porter unicode61'
        );

        -- Keep the FTS index in sync on delete; inserts are handled in code so
        -- that read-state updates do not trigger needless re-indexing.
        CREATE TRIGGER articles_fts_ad AFTER DELETE ON articles BEGIN
            DELETE FROM articles_fts WHERE rowid = old.id;
        END;

        CREATE TABLE settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
        ),
        // v2 — placeholder. An earlier sqlite-vec semantic-search schema was
        // removed; this keeps the version count aligned for databases that
        // already applied it. Search is keyword-only (FTS5).
        M::up("-- semantic search removed; search is FTS5 keyword-only"),
        // v3 — sync support: a remote item id per article plus a small queue
        // of local read/starred changes still to push to the sync server.
        M::up(
            r#"
            ALTER TABLE articles ADD COLUMN remote_id TEXT;
            CREATE TABLE sync_queue (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                field      TEXT NOT NULL,
                value      INTEGER NOT NULL,
                PRIMARY KEY (article_id, field)
            );
            "#,
        ),
        // v4 — article tags: a flat label set plus an article↔tag join table.
        M::up(
            r#"
            CREATE TABLE tags (
                id        INTEGER PRIMARY KEY,
                name      TEXT NOT NULL UNIQUE,
                color     TEXT NOT NULL DEFAULT 'clay',
                position  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE article_tags (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                tag_id     INTEGER NOT NULL REFERENCES tags(id)     ON DELETE CASCADE,
                PRIMARY KEY (article_id, tag_id)
            );
            CREATE INDEX idx_article_tags_tag ON article_tags(tag_id);
            "#,
        ),
        // v5 — filter rules: keyword matches applied to incoming articles to
        // auto-skip noise, or auto mark-read / star them, at ingestion time.
        M::up(
            r#"
            CREATE TABLE rules (
                id         INTEGER PRIMARY KEY,
                name       TEXT NOT NULL,
                enabled    INTEGER NOT NULL DEFAULT 1,
                feed_id    INTEGER REFERENCES feeds(id) ON DELETE CASCADE,
                field      TEXT NOT NULL DEFAULT 'title',
                query      TEXT NOT NULL,
                action     TEXT NOT NULL DEFAULT 'skip',
                position   INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        ),
        // v6 — index over the effective article date the list sorts by,
        // COALESCE(published_at, fetched_at), so a dateless entry sorts by
        // when it was fetched instead of sinking below every dated article.
        // (Superseded by v12, which rebuilds this index over a `datetime()`-
        // normalised expression — see that migration for why.)
        M::up(
            "CREATE INDEX idx_articles_sort
             ON articles(COALESCE(published_at, fetched_at) DESC, id DESC);",
        ),
        // v7 — every date ordering now sorts on the effective date and uses
        // idx_articles_sort, so the original published_at-only index is dead
        // weight on each insert. Drop it.
        M::up("DROP INDEX idx_articles_published;"),
        // v8 — partial indexes mirroring idx_articles_unread for the other
        // two smart-view flags, so the Starred / Read-later sidebar counts
        // and list queries use a tiny index instead of a full table scan.
        M::up(
            "CREATE INDEX idx_articles_starred
                 ON articles(is_starred) WHERE is_starred = 1;
             CREATE INDEX idx_articles_readlater
                 ON articles(read_later) WHERE read_later = 1;",
        ),
        // v9 — index the article URL. FreshRSS reconciliation matches remote
        // items to local articles by URL (up to ~1000 lookups per sync) and
        // the dedup check tests URL existence per inserted article; both
        // full-scanned the table without this.
        M::up("CREATE INDEX idx_articles_url ON articles(url);"),
        // v10 — email-newsletter sources (feature F5). A newsletter is a
        // normal `feeds` row (source_type = 'newsletter') so it lists,
        // searches and retains like an RSS feed; this side-table holds the
        // IMAP connection details, keyed 1:1 by feed_id and cascade-deleted
        // with the feed. The app-password is stored in plaintext, the same
        // way FreshRSS sync credentials live in the `settings` table — the
        // database never leaves the user's machine.
        M::up(
            r#"
            CREATE TABLE newsletter_sources (
                feed_id   INTEGER PRIMARY KEY REFERENCES feeds(id) ON DELETE CASCADE,
                host      TEXT NOT NULL,
                port      INTEGER NOT NULL DEFAULT 993,
                username  TEXT NOT NULL,
                password  TEXT NOT NULL,
                folder    TEXT NOT NULL DEFAULT 'INBOX'
            );
            "#,
        ),
        // v11 — highlights / annotations layer (feature F7). Each highlight
        // pins a span of an article's rendered plain text. `text_offset` is
        // the character offset of the quote, and `prefix` / `suffix` carry a
        // short context window for robust re-anchoring when the rendered text
        // shifts (e.g. after full-text extraction replaces a feed snippet).
        // `note` is an optional user annotation; `color` is a palette key.
        M::up(
            r#"
            CREATE TABLE highlights (
                id          INTEGER PRIMARY KEY,
                article_id  INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                quote       TEXT NOT NULL,
                prefix      TEXT NOT NULL DEFAULT '',
                suffix      TEXT NOT NULL DEFAULT '',
                text_offset INTEGER NOT NULL DEFAULT 0,
                color       TEXT NOT NULL DEFAULT 'yellow',
                note        TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_highlights_article ON highlights(article_id);
            "#,
        ),
        // v12 — rebuild the article-sort index over the *normalised* effective
        // date. `published_at` is RFC 3339 (`2024-01-15T10:30:00+00:00`, the
        // `T`-separated form `to_rfc3339` writes) while `fetched_at` uses
        // SQLite's space-separated form (`2024-01-15 10:30:00`). The old index
        // (v6) ordered on the *raw* `COALESCE(published_at, fetched_at)`, so a
        // string `<` compared the two formats byte-for-byte — and the `T`
        // (0x54) sorts after a space (0x20), making a dated article look up to
        // a day newer than a same-instant dateless one. A list mixing both
        // kinds of rows then came out subtly out of chronological order.
        //
        // Wrapping the effective date in `datetime()` parses both formats to
        // one canonical representation. The ORDER BY clauses are wrapped to
        // match (see `list_articles` / `digest_source` / `preview_rule`); an
        // index on the raw column can't serve a `datetime()`-wrapped sort, so
        // the index expression must be wrapped identically for the planner to
        // keep using it (verified with EXPLAIN QUERY PLAN — no temp B-tree).
        M::up(
            "DROP INDEX idx_articles_sort;
             CREATE INDEX idx_articles_sort
                 ON articles(datetime(COALESCE(published_at, fetched_at)) DESC,
                             id DESC);",
        ),
        // v13 — mark feeds whose title the user has set by hand. A refresh
        // pulls the feed document's own `<title>` and `update_feed_meta`
        // `COALESCE`s it over the stored one, which silently reverted a
        // manual rename on the very next poll. This flag lets `update_feed_meta`
        // leave a user-named feed's title alone while still refreshing every
        // other piece of feed metadata.
        M::up(
            "ALTER TABLE feeds ADD COLUMN custom_title INTEGER NOT NULL DEFAULT 0;",
        ),
        // v14 — cache a translated copy of the article body. `translated_lang`
        // records the target language the cache was produced for, so a later
        // change to the translation-target setting is detected as a cache miss.
        M::up(
            "ALTER TABLE articles ADD COLUMN translated_html TEXT;
             ALTER TABLE articles ADD COLUMN translated_lang TEXT;",
        ),
        // v15 — per-feed refresh interval (minutes). NULL follows the global
        // `refresh_interval_min` setting; the 525_600 sentinel means "never".
        M::up("ALTER TABLE feeds ADD COLUMN refresh_interval_min INTEGER;"),
        // v16 — per-feed auto-translate. 0 (the default) shows the original
        // text; 1 translates an article into the configured target language the
        // moment it is opened.
        M::up(
            "ALTER TABLE feeds ADD COLUMN auto_translate INTEGER NOT NULL DEFAULT 0;",
        ),
        // v17 — independent list-pane translation cache. Preview translations
        // are keyed by article, target language and engine so switching language
        // or engine never overwrites another cached preview. A source title/body
        // update invalidates the derived preview text.
        M::up(
            r#"
            CREATE TABLE article_preview_translations (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                lang       TEXT NOT NULL,
                engine     TEXT NOT NULL,
                title      TEXT NOT NULL,
                snippet    TEXT NOT NULL,
                PRIMARY KEY(article_id, lang, engine)
            );
            CREATE TRIGGER article_preview_translations_au
            AFTER UPDATE OF title, body_text ON articles BEGIN
                DELETE FROM article_preview_translations WHERE article_id = new.id;
            END;
            "#,
        ),
        // v18 — retention tombstones. A full-archive feed (Hugo's `index.xml`
        // ships the site's entire history) keeps every past item in the feed
        // document forever, so a read article that retention purges is re-fetched
        // on the very next refresh and re-inserted as brand-new *unread* —
        // resurfacing the whole archive the user just cleared, every day the
        // daily cleanup runs (issue #98). `cleanup_old_articles` records each
        // purged article's (feed_id, guid) here and `upsert_article` drops a
        // matching re-fetch, so a retention-purged read article stays gone.
        // Cascade-deletes with the feed.
        M::up(
            r#"
            CREATE TABLE article_tombstones (
                feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
                guid    TEXT NOT NULL,
                PRIMARY KEY (feed_id, guid)
            );
            "#,
        ),
        // v19 — per-feed open mode (issue #110): 'reader', 'extracted' or
        // 'web'. NULL (the default) keeps today's behaviour — reader view,
        // honouring the global auto-extract preference.
        M::up("ALTER TABLE feeds ADD COLUMN open_mode TEXT;"),
        // v20 — multi-user Web: accounts, sessions, per-user read/star/later,
        // and directory-index feed sources (auto-subscribe from Apache Index pages).
        M::up(
            r#"
            CREATE TABLE users (
                id            INTEGER PRIMARY KEY,
                username      TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                is_admin      INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE sessions (
                token      TEXT PRIMARY KEY,
                user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT
            );
            CREATE INDEX idx_sessions_user ON sessions(user_id);
            CREATE TABLE user_article_states (
                user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                is_read    INTEGER NOT NULL DEFAULT 0,
                is_starred INTEGER NOT NULL DEFAULT 0,
                read_later INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_id, article_id)
            );
            CREATE INDEX idx_uas_user_unread
                ON user_article_states(user_id) WHERE is_read = 0;
            CREATE INDEX idx_uas_user_starred
                ON user_article_states(user_id) WHERE is_starred = 1;
            CREATE INDEX idx_uas_user_later
                ON user_article_states(user_id) WHERE read_later = 1;
            CREATE TABLE feed_sources (
                id              INTEGER PRIMARY KEY,
                base_url        TEXT NOT NULL UNIQUE,
                last_checked_at TEXT
            );
            "#,
        ),
        // v21 — link directory-index feed sources to an auto-created folder so
        // feeds discovered from the same index land together in the sidebar.
        M::up(
            r#"
            ALTER TABLE feed_sources
                ADD COLUMN folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL;
            "#,
        ),
        // v22 — auto-tag queue: after ingest, a background worker asks the
        // configured LLM to suggest tags (reuse existing or create new).
        M::up(
            r#"
            CREATE TABLE auto_tag_queue (
                article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
                status     TEXT NOT NULL DEFAULT 'pending',
                attempts   INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_auto_tag_queue_status
                ON auto_tag_queue(status, created_at);
            "#,
        ),
        // v23 — AI usage accounting: one row per completed AI call (summarize /
        // Q&A / digest / translate / auto-tag), carrying the provider-reported
        // token counts so the app can show usage and estimated cost.
        M::up(
            r#"
            CREATE TABLE ai_usage (
                id                INTEGER PRIMARY KEY,
                feature           TEXT NOT NULL,
                provider          TEXT NOT NULL DEFAULT '',
                model             TEXT NOT NULL DEFAULT '',
                prompt_tokens     INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens  INTEGER NOT NULL DEFAULT 0,
                created_at        TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_ai_usage_created
                ON ai_usage(created_at);
            "#,
        ),
        // v24 — two tag taxonomies on one table: `interest` (admin closed
        // vocabulary, starts empty) and `ai` (free-form / legacy tags).
        // Pre-existing rows become AI tags so interest stays empty until the
        // admin curates it. Uniqueness is per (kind, name) so the same label
        // can exist in both taxonomies without colliding.
        M::up(
            r#"
            CREATE TABLE tags_new (
                id        INTEGER PRIMARY KEY,
                name      TEXT NOT NULL,
                color     TEXT NOT NULL DEFAULT 'clay',
                position  INTEGER NOT NULL DEFAULT 0,
                kind      TEXT NOT NULL DEFAULT 'interest',
                UNIQUE(kind, name)
            );
            INSERT INTO tags_new(id, name, color, position, kind)
                SELECT id, name, color, position, 'ai' FROM tags;
            DROP TABLE tags;
            ALTER TABLE tags_new RENAME TO tags;
            CREATE INDEX idx_tags_kind ON tags(kind);
            "#,
        ),
        // v25 — fixup for DBs that already applied an earlier v24 which
        // classified every legacy tag as `interest`. Interest should be empty
        // by default (admin-curated); move those rows to `ai`. When an AI tag
        // with the same name already exists, merge article links onto it and
        // drop the interest duplicate so UNIQUE(kind, name) is preserved.
        M::up(
            r#"
            INSERT OR IGNORE INTO article_tags(article_id, tag_id)
            SELECT at.article_id, a.id
            FROM article_tags at
            JOIN tags i ON i.id = at.tag_id AND i.kind = 'interest'
            JOIN tags a ON a.kind = 'ai' AND a.name = i.name;

            DELETE FROM tags
            WHERE kind = 'interest'
              AND name IN (SELECT name FROM tags WHERE kind = 'ai');

            UPDATE tags SET kind = 'ai' WHERE kind = 'interest';
            "#,
        ),
        // v26 — word-cloud term pre-aggregation. Tokenize title+summary once at
        // ingest (and via admin/startup backfill); the wordcloud API aggregates
        // from `article_terms` over a calendar-day window instead of re-scanning
        // full text on every request.
        M::up(
            r#"
            CREATE TABLE article_terms (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                term       TEXT NOT NULL,
                group_key  TEXT NOT NULL DEFAULT 'general',
                weight     REAL NOT NULL DEFAULT 1,
                day        TEXT NOT NULL,
                PRIMARY KEY (article_id, term)
            );
            CREATE INDEX idx_article_terms_day_term
                ON article_terms(day, term);
            CREATE INDEX idx_article_terms_day
                ON article_terms(day);

            CREATE TABLE article_term_index (
                article_id   INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
                dict_version INTEGER NOT NULL DEFAULT 0,
                updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_article_term_index_dict
                ON article_term_index(dict_version);
            "#,
        ),
        // v27 — auto-tag claim index: workers filter by status then join
        // articles for newest-first ordering; keep a dedicated status index.
        M::up(
            r#"
            DROP INDEX IF EXISTS idx_auto_tag_queue_status;
            CREATE INDEX idx_auto_tag_queue_status
                ON auto_tag_queue(status, updated_at);
            "#,
        ),
        // v28 — DeepSeek-style cache-hit accounting: input tokens that hit the
        // provider context cache are billed cheaper than cache-miss input.
        M::up(
            r#"
            ALTER TABLE ai_usage ADD COLUMN cache_hit_tokens INTEGER NOT NULL DEFAULT 0;
            "#,
        ),
        // v29 — global article URL uniqueness ("smart dedupe"). Multiple feeds
        // often push the same story link; previously only a soft EXISTS check
        // gated by `dedup_enabled` (default off) prevented duplicates, so
        // cross-feed same-URL rows piled up. Clean existing duplicates keeping
        // the earliest-fetched row (lowest id), replace the non-unique URL
        // index with a partial UNIQUE index (empty/NULL URLs stay unrestricted),
        // and turn the setting on for databases that never set it.
        M::up(
            r#"
            DELETE FROM articles
            WHERE id IN (
                SELECT id FROM (
                    SELECT a.id AS id
                    FROM articles a
                    WHERE a.url IS NOT NULL AND a.url != ''
                      AND a.id NOT IN (
                          SELECT MIN(id) FROM articles
                          WHERE url IS NOT NULL AND url != ''
                          GROUP BY url
                      )
                )
            );

            DROP INDEX IF EXISTS idx_articles_url;
            CREATE UNIQUE INDEX idx_articles_url
                ON articles(url)
                WHERE url IS NOT NULL AND url != '';

            INSERT OR IGNORE INTO settings(key, value)
                VALUES ('dedup_enabled', '1');
            "#,
        ),
        // v30 — interest-tag aliases: admin-maintained synonym → canonical tag.
        // Additive only (new table); production DBs open → migrate with an
        // empty alias table — no backfill. Deleting a tag cascades its aliases.
        // Do NOT drop/rebuild `tags` or `article_tags`. Owned inside papr
        // (Settings), not wordcloud entity JSON.
        M::up(
            r#"
            CREATE TABLE tag_aliases (
                id     INTEGER PRIMARY KEY,
                alias  TEXT NOT NULL,
                tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                kind   TEXT NOT NULL,
                UNIQUE(kind, alias COLLATE NOCASE)
            );
            CREATE INDEX idx_tag_aliases_tag ON tag_aliases(tag_id);
            "#,
        ),
        // v31 — official DeepSeek balance snapshots + dashboard usage, so the
        // admin UI can show the *real* money spent (balance deltas from the
        // official /user/balance endpoint) next to the locally-estimated cost.
        // Additive only; populated by a background job and on-demand refresh.
        // `ai_official_usage` is filled from the (undocumented, best-effort)
        // platform dashboard endpoints when a platform session token is
        // configured — it is optional and degrades gracefully when absent.
        M::up(
            r#"
            CREATE TABLE ai_balance_history (
                id              INTEGER PRIMARY KEY,
                recorded_at     TEXT NOT NULL UNIQUE,
                total_balance   REAL NOT NULL,
                granted_balance REAL NOT NULL DEFAULT 0,
                topped_up_balance REAL NOT NULL DEFAULT 0
            );
            CREATE TABLE ai_official_usage (
                id      INTEGER PRIMARY KEY,
                day     TEXT NOT NULL UNIQUE,
                tokens  INTEGER NOT NULL DEFAULT 0,
                cost    REAL NOT NULL DEFAULT 0
            );
            "#,
        ),
    ])
});

/// Register Papr's custom SQL scalar functions on a freshly opened connection.
///
/// SQLite's built-in `LOWER()` only case-folds ASCII (it has no Unicode
/// awareness without the ICU extension, which the bundled build omits). Rust's
/// `str::to_lowercase()` is fully Unicode-aware. Anywhere a query needs to
/// match the case-folding the Rust code does — notably `preview_rule`, which
/// must agree with `rule_matches`'s `to_lowercase()` so the rule preview counts
/// exactly the articles live ingestion would act on — `unicode_lower` provides
/// it. SQLite scalar functions are per-connection, so this runs for every
/// connection (the writer and each pooled reader).
fn register_functions(conn: &Connection) -> AppResult<()> {
    conn.create_scalar_function(
        "unicode_lower",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            // A NULL argument folds to NULL; callers `COALESCE` beforehand, but
            // staying total here keeps the function safe to use bare.
            let value: Option<String> = ctx.get(0)?;
            Ok(value.map(|s| s.to_lowercase()))
        },
    )?;
    Ok(())
}

/// Open the writer connection: run migrations and set the write-side pragmas.
/// WAL mode is persisted in the database header, so reader connections opened
/// afterwards inherit it automatically.
pub fn open(path: &Path) -> AppResult<Connection> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    MIGRATIONS.to_latest(&mut conn)?;
    register_functions(&conn)?;
    Ok(conn)
}

/// Open a read-only connection for the UI query pool. Under WAL these run
/// concurrently with the writer, so interface reads never block on a
/// background refresh. `query_only` is a safety net against an accidental
/// write on a pooled reader. Must be called after `open` has migrated.
pub fn open_reader(path: &Path) -> AppResult<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", true)?;
    register_functions(&conn)?;
    Ok(conn)
}

// ─────────────────────────── folders ───────────────────────────

pub fn list_folders(conn: &Connection) -> AppResult<Vec<Folder>> {
    let mut stmt =
        conn.prepare("SELECT id, name, position FROM folders ORDER BY position, name")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                name: r.get(1)?,
                position: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Create a folder, or return the existing one when a folder with the same
/// name (case-insensitively) is already present.
///
/// `folders.name` carries no `UNIQUE` constraint, so without this guard two
/// folders named "Tech" — or "Tech" and "tech" — could coexist, leaving the
/// sidebar with confusing near-duplicates. Idempotent-on-name is also exactly
/// what `folder_id_by_name` (and so OPML import) wants: a feed nested under a
/// folder whose name already exists must land in *that* folder rather than a
/// freshly created twin. This mirrors `create_tag`'s case-insensitive dedup.
pub fn create_folder(conn: &Connection, name: &str) -> AppResult<i64> {
    // Trim before the dedup lookup and the insert: a name carrying surrounding
    // whitespace (a pasted OPML `<outline text=" Tech ">`, an accidental
    // trailing space) is a different string from its trimmed twin, so the
    // `COLLATE NOCASE` lookup below would miss the existing folder and spawn
    // the near-duplicate the dedup exists to prevent. Normalising here — the
    // one chokepoint every caller (UI prompt, OPML import) funnels through —
    // keeps the invariant independent of any caller-side trimming.
    let name = name.trim();
    // Reject an empty/whitespace-only name at the same chokepoint. The
    // `PromptDialog` guards the interactive path, but `import_opml` reaches
    // this through `folder_id_by_name` with no such guard: an OPML folder
    // outline labelled with only whitespace (or an empty `text`/`title`
    // attribute) would otherwise insert a blank-named folder into the sidebar,
    // indistinguishable from a glitch and impossible to tell apart from any
    // other blank folder. Mirrors the guard `rename_feed` already applies.
    if name.is_empty() {
        return Err(AppError::code("emptyFolderName"));
    }
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM folders WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO folders(name, position) VALUES (?1, (SELECT COALESCE(MAX(position),0)+1 FROM folders))",
        params![name],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename a folder, rejecting a name that collides with a *different* folder.
///
/// `create_folder` collapses same-name folders, so a rename onto an existing
/// folder's name (or a case variant of it) would otherwise recreate exactly
/// the near-duplicate that dedup prevents — leaving the two functions
/// inconsistent. Match case-insensitively and return the localisable
/// `folderNameExists` code. Renaming a folder to its own name (or a case
/// change of it) is allowed. Mirrors `rename_tag`.
pub fn rename_folder(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    // Trim so the collision check and the stored value match what `create_folder`
    // would produce — otherwise a rename to `" Tech "` slips past the clash test
    // against an existing `"Tech"` and recreates the near-duplicate.
    let name = name.trim();
    // Reject an empty/whitespace-only name, the same guard `create_folder` and
    // `rename_feed` apply: a rename to a blank string would leave the folder
    // unlabelled in the sidebar with no recovery path short of renaming it
    // again to something valid.
    if name.is_empty() {
        return Err(AppError::code("emptyFolderName"));
    }
    let clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM folders WHERE name = ?1 COLLATE NOCASE AND id != ?2",
            params![name, id],
            |r| r.get(0),
        )
        .optional()?;
    if clash.is_some() {
        return Err(AppError::code("folderNameExists"));
    }
    conn.execute("UPDATE folders SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn delete_folder(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
    Ok(())
}

// ─────────────────────────── feeds ───────────────────────────

pub fn find_feed_by_url(conn: &Connection, url: &str) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM feeds WHERE feed_url = ?1", params![url], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn insert_feed(
    conn: &Connection,
    feed_url: &str,
    site_url: Option<&str>,
    title: &str,
    description: Option<&str>,
    source_type: SourceType,
    folder_id: Option<i64>,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO feeds(feed_url, site_url, title, description, source_type, folder_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![feed_url, site_url, title, description, source_type.as_str(), folder_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Promote a feed's `source_type` once its real kind is known from the parsed
/// document — but only when it is still the generic `'rss'`.
///
/// `add_feed` classifies a feed precisely (it has the parsed feed in hand and
/// runs `parse::refine_source_type`), but `import_opml` can only call
/// `parse::detect_source_type`, which inspects the URL alone and so cannot see
/// that a feed is a podcast (audio enclosures) or a Mastodon timeline. An
/// OPML-imported podcast therefore stays mislabelled `'rss'` forever, losing
/// its source badge and podcast-specific UI. The refresh loop calls this on
/// every successful fetch to correct such feeds from their first poll onward.
///
/// The `WHERE source_type = 'rss'` guard makes this strictly a promotion: a
/// feed already classified (youtube / bluesky / podcast / mastodon / reddit /
/// newsletter) is never touched, so a re-poll cannot demote or churn the type.
pub fn refine_feed_source_type(
    conn: &Connection,
    id: i64,
    source_type: SourceType,
) -> AppResult<()> {
    if source_type == SourceType::Rss {
        return Ok(());
    }
    conn.execute(
        "UPDATE feeds SET source_type = ?2
         WHERE id = ?1 AND source_type = 'rss'",
        params![id, source_type.as_str()],
    )?;
    Ok(())
}

pub fn list_feeds(conn: &Connection) -> AppResult<Vec<Feed>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.feed_url, f.site_url, f.title, f.description, f.favicon_url,
                f.folder_id, f.source_type, f.last_fetched_at, f.fetch_error,
                (SELECT COUNT(*) FROM articles a WHERE a.feed_id = f.id AND a.is_read = 0),
                f.refresh_interval_min, f.auto_translate, f.open_mode
         FROM feeds f ORDER BY f.title COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
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

/// A feed the scheduler should fetch: `(id, feed_url, etag, last_modified)` —
/// the last two are the stored revalidators for a conditional GET.
pub type FeedToRefresh = (i64, String, Option<String>, Option<String>);

/// All feeds that need an HTTP fetch. Newsletter sources are excluded — they
/// are polled over IMAP separately (see `scheduler::poll_newsletters`); their
/// synthetic `imap://` feed_url is not an HTTP-fetchable document.
pub fn feeds_to_refresh(conn: &Connection) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter'",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The "never auto-refresh" sentinel (minutes ≈ one year). The Settings panel
/// writes it as the global interval when auto-refresh is switched off, and a
/// per-feed interval can carry it to opt one feed out of automatic refresh.
pub const REFRESH_OFF_MINUTES: i64 = 525_600;

/// Non-newsletter feeds that are *due* for a fetch: their effective interval —
/// the per-feed `refresh_interval_min`, or `global_min` when unset — has
/// elapsed since `last_fetched_at` (a never-fetched feed is always due). Feeds
/// whose effective interval is the "off" sentinel are excluded entirely. Used
/// by the background scheduler; the manual refresh still fetches every feed via
/// `feeds_to_refresh`.
pub fn feeds_due_for_refresh(
    conn: &Connection,
    global_min: i64,
) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter'
           AND COALESCE(refresh_interval_min, ?1) < ?2
           AND ( last_fetched_at IS NULL
                 OR (julianday('now') - julianday(last_fetched_at)) * 1440.0
                    >= COALESCE(refresh_interval_min, ?1) )",
    )?;
    let rows = stmt
        .query_map(params![global_min, REFRESH_OFF_MINUTES], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Non-newsletter feeds for a single feed id — the per-feed manual refresh.
/// An empty result means the id is unknown or names a newsletter source (which
/// is polled over IMAP, not fetched here).
pub fn feeds_to_refresh_for_feed(conn: &Connection, feed_id: i64) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter' AND id = ?1",
    )?;
    let rows = stmt
        .query_map(params![feed_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Non-newsletter feeds in a folder — the per-folder manual refresh.
pub fn feeds_to_refresh_in_folder(
    conn: &Connection,
    folder_id: i64,
) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter' AND folder_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![folder_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Set (or clear) a feed's per-feed refresh interval. `None` reverts the feed
/// to the global interval; `Some(REFRESH_OFF_MINUTES)` opts it out entirely.
pub fn set_feed_refresh_interval(
    conn: &Connection,
    id: i64,
    minutes: Option<i64>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET refresh_interval_min = ?2 WHERE id = ?1",
        params![id, minutes],
    )?;
    Ok(())
}

/// Toggle a feed's per-feed auto-translate flag. When on, opening an article
/// from this feed starts a translation into the configured target language.
pub fn set_feed_auto_translate(conn: &Connection, id: i64, enabled: bool) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET auto_translate = ?2 WHERE id = ?1",
        params![id, enabled as i64],
    )?;
    Ok(())
}

/// Set (or clear) a feed's per-feed open mode: `"reader"`, `"extracted"` or
/// `"web"`. `None` reverts the feed to the default behaviour.
pub fn set_feed_open_mode(conn: &Connection, id: i64, mode: Option<&str>) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET open_mode = ?2 WHERE id = ?1",
        params![id, mode],
    )?;
    Ok(())
}

/// Refresh a feed's metadata from its parsed document. A `None` *or empty*
/// field leaves the stored value untouched. The feed-supplied `title` is
/// applied only when the user has *not* renamed the feed by hand
/// (`custom_title = 0`); otherwise `update_feed_meta` would revert a manual
/// rename on the next poll.
///
/// Empty strings are treated exactly like `None` (`NULLIF(?, '')`): `feed-rs`
/// parses a `<title></title>` element as `Some("")`, and the scheduler's
/// refresh path passes the parsed title straight through. Without the
/// `NULLIF` guard, a feed that momentarily serves an empty `<title>` would
/// overwrite a perfectly good feed name with a blank string in the sidebar —
/// `COALESCE` only skips a SQL `NULL`, not an empty string. `add_feed`
/// already filters empty titles on the subscribe path; this makes the
/// periodic-refresh path just as safe, and applies the same protection to
/// the other metadata columns.
pub fn update_feed_meta(
    conn: &Connection,
    id: i64,
    title: Option<&str>,
    site_url: Option<&str>,
    description: Option<&str>,
    favicon_url: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET
            title       = CASE WHEN custom_title = 1 THEN title
                               ELSE COALESCE(NULLIF(?2, ''), title) END,
            site_url    = COALESCE(NULLIF(?3, ''), site_url),
            description = COALESCE(NULLIF(?4, ''), description),
            favicon_url = COALESCE(NULLIF(?5, ''), favicon_url)
         WHERE id = ?1",
        params![id, title, site_url, description, favicon_url],
    )?;
    Ok(())
}

pub fn set_feed_fetch_state(
    conn: &Connection,
    id: i64,
    etag: Option<&str>,
    last_modified: Option<&str>,
    error: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET etag = ?2, last_modified = ?3, fetch_error = ?4,
                          last_fetched_at = datetime('now')
         WHERE id = ?1",
        params![id, etag, last_modified, error],
    )?;
    Ok(())
}

/// Record a successful fetch that produced no changes (304 Not Modified).
pub fn touch_feed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET last_fetched_at = datetime('now'), fetch_error = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// A single feed's `last_fetched_at` timestamp, if it has ever been fetched.
pub fn feed_last_fetched(conn: &Connection, id: i64) -> AppResult<Option<String>> {
    Ok(conn.query_row(
        "SELECT last_fetched_at FROM feeds WHERE id = ?1",
        params![id],
        |r| r.get::<_, Option<String>>(0),
    )?)
}

/// How many articles a feed currently holds. Used to detect a feed's *first*
/// ingestion (zero articles) so the initial backfill can be depth-capped —
/// a brand-new feed's first fetch otherwise ingests its whole history, and
/// every historical item then triggers an auto-tag LLM call.
pub fn feed_article_count(conn: &Connection, feed_id: i64) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM articles WHERE feed_id = ?1",
        params![feed_id],
        |r| r.get(0),
    )?)
}

/// Record a failed fetch, keeping the previous content untouched.
pub fn set_feed_error(conn: &Connection, id: i64, error: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET last_fetched_at = datetime('now'), fetch_error = ?2 WHERE id = ?1",
        params![id, error],
    )?;
    Ok(())
}

pub fn delete_feed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM feeds WHERE id = ?1", params![id])?;
    Ok(())
}

/// Feeds for OPML export as `(title, feed_url, folder)` tuples. Newsletter
/// sources are excluded: OPML is an RSS-subscription interchange format, and a
/// newsletter's `feed_url` is a synthetic `imap://user@host:port/folder`
/// string — exporting it would emit an `<outline xmlUrl="imap://…">` that any
/// reader (Papr's own `import_opml` included) would treat as an RSS feed and
/// then fail to HTTP-fetch forever, with the IMAP credentials not even carried.
pub fn feeds_for_export(conn: &Connection) -> AppResult<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT f.title, f.feed_url, fo.name
         FROM feeds f LEFT JOIN folders fo ON fo.id = f.folder_id
         WHERE f.source_type != 'newsletter'
         ORDER BY fo.name, f.title",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Feed URLs the sync layer should mirror onto the server. Excludes
/// `newsletter` sources (those have no upstream feed URL the server could
/// subscribe to), matching the OPML-export filter.
pub fn feed_urls_for_sync(conn: &Connection) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT feed_url FROM feeds WHERE source_type != 'newsletter' AND feed_url <> ''",
    )?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Find a folder by name, creating it if absent. Used during OPML import.
/// Resolve a folder name to its id, creating the folder when absent. Used by
/// OPML import to attach imported feeds to their folders. `create_folder` is
/// itself case-insensitively idempotent, so an OPML folder whose name matches
/// an existing folder (in any case) reuses that folder instead of spawning a
/// near-duplicate.
pub fn folder_id_by_name(conn: &Connection, name: &str) -> AppResult<i64> {
    create_folder(conn, name)
}

pub fn move_feed(conn: &Connection, id: i64, folder_id: Option<i64>) -> AppResult<()> {
    conn.execute("UPDATE feeds SET folder_id = ?2 WHERE id = ?1", params![id, folder_id])?;
    Ok(())
}

/// The folder a feed currently sits in, if any. Used by sync to tell an
/// already-filed feed (leave it) from an unfiled one (adopt the server folder).
pub fn feed_folder_id(conn: &Connection, id: i64) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT folder_id FROM feeds WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Set a feed's display title to a user-chosen value. `custom_title` is also
/// raised so a later refresh's `update_feed_meta` does not revert the rename
/// back to the feed document's own `<title>`.
pub fn rename_feed(conn: &Connection, id: i64, title: &str) -> AppResult<()> {
    // Reject an empty/whitespace-only title at the chokepoint. A rename also
    // sets `custom_title = 1`, which makes `update_feed_meta`
    // never again overwrite the title from the feed document — so an empty
    // title would leave the feed *permanently* blank in the sidebar with no
    // recovery path, not even a refresh. The frontend `PromptDialog` guards
    // against this, but the backend command is the real chokepoint (other IPC
    // callers exist), so enforce it here the way `rename_tag` already does.
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::code("emptyFeedTitle"));
    }
    conn.execute(
        "UPDATE feeds SET title = ?2, custom_title = 1 WHERE id = ?1",
        params![id, title],
    )?;
    Ok(())
}

// ─────────────────────────── newsletter sources ───────────────────────────

/// One configured email-newsletter source: the backing feed plus its IMAP
/// connection details. Mirrors the `commands::NewsletterSource` payload.
pub struct NewsletterSourceRow {
    pub feed_id: i64,
    pub title: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub folder: String,
}

/// Insert a newsletter source: a `feeds` row (source_type = 'newsletter') plus
/// its IMAP credentials in `newsletter_sources`. Both land in one transaction
/// so a failure cannot leave a feed with no credentials. Returns the feed id.
pub fn insert_newsletter_source(
    conn: &Connection,
    feed_url: &str,
    title: &str,
    cfg: &crate::ingestion::newsletter::NewsletterConfig,
) -> AppResult<i64> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO feeds(feed_url, title, source_type) VALUES (?1, ?2, 'newsletter')",
        params![feed_url, title],
    )?;
    let feed_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO newsletter_sources(feed_id, host, port, username, password, folder)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![feed_id, cfg.host, cfg.port, cfg.username, cfg.password, cfg.folder],
    )?;
    tx.commit()?;
    Ok(feed_id)
}

/// Every configured newsletter source (without the password) for the UI list.
pub fn list_newsletter_sources(conn: &Connection) -> AppResult<Vec<NewsletterSourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT n.feed_id, f.title, n.host, n.port, n.username, n.folder
         FROM newsletter_sources n JOIN feeds f ON f.id = n.feed_id
         ORDER BY f.title COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(NewsletterSourceRow {
                feed_id: r.get(0)?,
                title: r.get(1)?,
                host: r.get(2)?,
                port: r.get::<_, i64>(3)? as u16,
                username: r.get(4)?,
                folder: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for every newsletter source — the work list the
/// refresh scheduler polls each cycle. Returning a `NewsletterConfig` directly
/// spares the caller a field-by-field rebuild.
pub fn newsletter_sources_to_poll(
    conn: &Connection,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT feed_id, host, port, username, password, folder FROM newsletter_sources",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for newsletter sources that are *due* to be polled,
/// applying the same per-feed/global interval logic as `feeds_due_for_refresh`.
/// Used by the background scheduler; the manual refresh polls every mailbox.
pub fn newsletter_sources_due_to_poll(
    conn: &Connection,
    global_min: i64,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT s.feed_id, s.host, s.port, s.username, s.password, s.folder
         FROM newsletter_sources s JOIN feeds f ON f.id = s.feed_id
         WHERE COALESCE(f.refresh_interval_min, ?1) < ?2
           AND ( f.last_fetched_at IS NULL
                 OR (julianday('now') - julianday(f.last_fetched_at)) * 1440.0
                    >= COALESCE(f.refresh_interval_min, ?1) )",
    )?;
    let rows = stmt
        .query_map(params![global_min, REFRESH_OFF_MINUTES], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for a single newsletter source — the per-feed
/// manual refresh. Empty when the feed id is not a newsletter source.
pub fn newsletter_sources_for_feed(
    conn: &Connection,
    feed_id: i64,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT feed_id, host, port, username, password, folder
         FROM newsletter_sources WHERE feed_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![feed_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for the newsletter sources in a folder — the
/// per-folder manual refresh.
pub fn newsletter_sources_in_folder(
    conn: &Connection,
    folder_id: i64,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT s.feed_id, s.host, s.port, s.username, s.password, s.folder
         FROM newsletter_sources s JOIN feeds f ON f.id = s.feed_id
         WHERE f.folder_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![folder_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Remove a newsletter source. Deleting the `feeds` row cascades to both
/// `newsletter_sources` and the source's articles.
pub fn delete_newsletter_source(conn: &Connection, feed_id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM feeds WHERE id = ?1", params![feed_id])?;
    Ok(())
}

// ─────────────────────────── articles ───────────────────────────

/// A parsed article ready for insertion.
pub struct NewArticle {
    pub guid: String,
    pub url: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub content_html: Option<String>,
    pub body_text: String,
    pub image_url: Option<String>,
    pub published_at: Option<String>,
    pub enclosures: Vec<Enclosure>,
}

/// Shrink `articles` to the `cap` most recent items by `published_at`, keeping
/// the original document order for the survivors. Items without a publish date
/// sort as oldest, so a feed whose XML is oldest-first still keeps its newest
/// entries (unlike a blind first-N truncation). `cap == 0` is a no-op.
///
/// Feed XML timestamps here are RFC3339 from `parse::clamp_publish_date`, all
/// produced by the same formatter, so lexicographic comparison is
/// chronological; items with unparseable dates fall back to string order,
/// which is harmless for a backfill cap.
pub fn cap_newest_articles(articles: &mut Vec<NewArticle>, cap: usize) {
    if cap == 0 || articles.len() <= cap {
        return;
    }
    let mut order: Vec<(Option<String>, usize)> = articles
        .iter()
        .enumerate()
        .map(|(i, a)| (a.published_at.clone(), i))
        .collect();
    // Newest first; missing date sorts last.
    order.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let mut keep = vec![false; articles.len()];
    for (_, i) in order.iter().take(cap) {
        keep[*i] = true;
    }
    let mut idx = 0usize;
    articles.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
}

/// True if `rule` (scoped to `feed_id`) matches the incoming article `a`.
/// The query is a comma-separated keyword list; any substring hit fires it.
fn rule_matches(rule: &Rule, feed_id: i64, a: &NewArticle) -> bool {
    if rule.feed_id.is_some_and(|fid| fid != feed_id) {
        return false;
    }
    let author = a.author.as_deref().unwrap_or("");
    // The fields the rule searches. `any` checks each field *independently*
    // (mirroring `preview_rule`'s per-column LIKE): a keyword must lie wholly
    // within one field. Concatenating the fields would let a keyword straddle
    // a field boundary (e.g. a title ending in "machine" + a body starting
    // with "learning" matching "machine learning"), so live ingestion would
    // act on articles the rule preview never counted.
    let fields: Vec<String> = match rule.field.as_str() {
        "author" => vec![author.to_lowercase()],
        "content" => vec![a.body_text.to_lowercase()],
        "any" => vec![
            a.title.to_lowercase(),
            author.to_lowercase(),
            a.body_text.to_lowercase(),
        ],
        _ => vec![a.title.to_lowercase()],
    };
    rule.query
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .any(|term| fields.iter().any(|h| h.contains(&term)))
}

/// Insert an article if it is new (by feed_id + guid). Returns `true` only when
/// a genuinely **new and unread** article was inserted — callers tally this as
/// the count of fresh articles surfaced to the user (refresh toast, "new
/// articles" notification, `add_newsletter_source`'s `unread_count`).
///
/// An article inserted but pre-marked read by a `read` rule returns `false`:
/// the row landed, but it never shows up as unread, so counting it would
/// inflate the "N new articles" figure and disagree with the sidebar's unread
/// count (the same overcount `add_feed` guards against).
///
/// Cross-feed URL first-win: a non-empty URL that already exists (in any feed)
/// is skipped — the earliest-fetched row keeps its `feed_id`. Empty/NULL URLs
/// are unrestricted. This is always applied (backed by a partial UNIQUE index);
/// `dedup` is retained for call-site compatibility and mirrors the
/// `dedup_enabled` setting callers pass in. Enabled `rules` are evaluated
/// first: a `skip` match drops the article entirely, while `read` / `star`
/// matches pre-set the article's state on insert.
pub fn upsert_article(
    conn: &Connection,
    feed_id: i64,
    a: &NewArticle,
    dedup: bool,
    rules: &[Rule],
) -> AppResult<bool> {
    // Trim so whitespace-only links don't occupy the unique URL slot, and so
    // `" https://…"` matches an already-stored `"https://…"`.
    let url = a
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    // First-win URL dedupe. Always enforced for non-empty URLs (unique index);
    // when `dedup` is false the soft EXISTS check is skipped but
    // `ON CONFLICT DO NOTHING` still collapses a racing duplicate insert.
    if dedup {
        if let Some(url) = url {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM articles WHERE url = ?1)",
                params![url],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(false);
            }
        }
    }
    // A previously retention-purged article (same feed + guid) must not be
    // re-ingested as fresh unread. A full-archive feed keeps every past item in
    // its document, so without this the daily cleanup and the next refresh would
    // ping-pong the whole read history back as new, every day (issue #98).
    // Checked regardless of the `dedup` flag — the tombstone, not URL dedup, is
    // what closes the loop.
    let tombstoned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM article_tombstones WHERE feed_id = ?1 AND guid = ?2)",
        params![feed_id, a.guid],
        |r| r.get(0),
    )?;
    if tombstoned {
        return Ok(false);
    }
    // Apply filter rules: skip wins outright; read / star tint the new row.
    let (mut start_read, mut start_starred) = (false, false);
    for rule in rules {
        if !rule_matches(rule, feed_id, a) {
            continue;
        }
        match rule.action.as_str() {
            "skip" => return Ok(false),
            "read" => start_read = true,
            "star" => start_starred = true,
            _ => {}
        }
    }
    // The article row, its FTS index entry, and its enclosures must land
    // together — a partial insert leaves an unsearchable or enclosure-less
    // article. Wrap them in a transaction so a mid-loop failure rolls back.
    let tx = conn.unchecked_transaction()?;
    // Bare ON CONFLICT DO NOTHING covers both UNIQUE(feed_id, guid) and the
    // partial unique URL index — first-win skip for either key.
    let n = tx.execute(
        "INSERT INTO articles
            (feed_id, guid, url, title, author, summary, content_html, body_text,
             image_url, published_at, is_read, is_starred)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT DO NOTHING",
        params![
            feed_id, a.guid, url, a.title, a.author, a.summary,
            a.content_html, a.body_text, a.image_url, a.published_at,
            start_read, start_starred
        ],
    )?;
    if n == 0 {
        return Ok(false);
    }
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO articles_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
        params![id, a.title, a.body_text],
    )?;
    for e in &a.enclosures {
        tx.execute(
            "INSERT INTO enclosures(article_id, url, mime_type, length) VALUES (?1,?2,?3,?4)",
            params![id, e.url, e.mime_type, e.length],
        )?;
    }
    // Enqueue for AI auto-tagging only when at least one tag feature is on.
    // Avoids growing a useless backlog while both toggles are off. Same
    // transaction as the insert so a crash cannot strand an untagged row.
    let tag_enabled = setting_flag(&*tx, "auto_tag_enabled", false)
        || setting_flag(&*tx, "ai_tag_enabled", false);
    // Historical backfill is the token-cost spike source: a newly-added feed's
    // first fetch ingests its whole history, and every old item would trigger
    // a full-price LLM call. Skip enqueueing items published more than
    // `ai_tag_max_age_days` ago (default 3; 0 = no age gate). Items without a
    // parseable date are treated as fresh — they cost one call, not hundreds.
    let stale = {
        let max_age_days = setting_parsed::<i64>(&*tx, "ai_tag_max_age_days", 3);
        max_age_days > 0
            && a.published_at.as_deref().is_some_and(|p| {
                chrono::DateTime::parse_from_rfc3339(p)
                    .map(|dt| dt + chrono::Duration::days(max_age_days) < chrono::Utc::now())
                    .unwrap_or(false)
            })
    };
    if tag_enabled && !stale {
        tx.execute(
            "INSERT INTO auto_tag_queue(article_id, status, attempts, last_error, updated_at)
             VALUES (?1, 'pending', 0, NULL, datetime('now'))
             ON CONFLICT(article_id) DO UPDATE SET
                 status = 'pending',
                 attempts = 0,
                 last_error = NULL,
                 updated_at = datetime('now')",
            params![id],
        )?;
    }
    // Word-cloud terms: tokenize title+summary once so the cloud API can
    // aggregate from `article_terms` instead of re-scanning text per request.
    // Failures are logged inside the helper and must not roll back ingest.
    let summary = a.summary.as_deref().unwrap_or("");
    let body_snip: String = a.body_text.chars().take(400).collect();
    let snippet = if summary.is_empty() {
        body_snip.as_str()
    } else {
        summary
    };
    if let Err(e) = crate::wordcloud::index_article_snippet(
        &*tx,
        id,
        &a.title,
        snippet,
        a.published_at.as_deref(),
        None,
    ) {
        log::warn!("wordcloud index failed (article {id}): {e}");
    }
    tx.commit()?;
    // A row inserted but pre-marked read by a `read` rule is not "new" from
    // the user's point of view — report it as not-inserted so it is excluded
    // from new-article tallies.
    Ok(!start_read)
}

/// Build and run the article-list query for the given sidebar selection.
/// WHERE clauses + bind values selecting the articles for `query` under the
/// unread filter — shared by `list_articles` and `article_index` so the two
/// never drift in *which* rows they consider (a drift would put a located
/// article at an index the list doesn't agree with). The returned clauses
/// assume the `articles a JOIN feeds f` aliases used by both callers.
fn article_filter(query: &ArticleQuery, unread_only: bool) -> (Vec<String>, Vec<Value>) {
    let mut where_clauses: Vec<String> = vec!["1=1".into()];
    let mut binds: Vec<Value> = Vec::new();

    match query {
        ArticleQuery::All => {}
        ArticleQuery::Unread => where_clauses.push("a.is_read = 0".into()),
        ArticleQuery::Starred => where_clauses.push("a.is_starred = 1".into()),
        ArticleQuery::ReadLater => where_clauses.push("a.read_later = 1".into()),
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
        where_clauses.push("a.is_read = 0".into());
    }
    (where_clauses, binds)
}

/// The ORDER BY clause shared by `list_articles` and `article_index`.
/// When `rank_first` is set (active search + relevance sort), FTS5 `bm25`
/// rank leads and the browse date order is secondary.
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

/// 0-based position of `article_id` within the list `query` would produce under
/// the same unread filter and sort — or `None` when the article isn't in that
/// list (filtered out, or not in this feed/folder/tag). Lets the frontend page
/// the virtual list up to a specific article (e.g. one opened from search that
/// lives far below the first loaded page) and scroll it into view.
pub fn article_index(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
    oldest_first: bool,
    article_id: i64,
) -> AppResult<Option<i64>> {
    let (where_clauses, mut binds) = article_filter(query, unread_only);
    let sql = format!(
        "SELECT pos FROM (
             SELECT a.id AS aid,
                    ROW_NUMBER() OVER (ORDER BY {order}) - 1 AS pos
             FROM articles a JOIN feeds f ON f.id = a.feed_id
             WHERE {where_sql}
         ) WHERE aid = ?",
        order = article_order(oldest_first, false),
        where_sql = where_clauses.join(" AND "),
    );
    binds.push(Value::Integer(article_id));
    let pos = conn
        .prepare(&sql)?
        .query_row(params_from_iter(binds), |r| r.get::<_, i64>(0))
        .optional()?;
    Ok(pos)
}

pub fn list_articles(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
    search: Option<&str>,
    oldest_first: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    list_articles_sorted(
        conn,
        query,
        unread_only,
        search,
        oldest_first,
        /* sort_by_relevance */ true,
        limit,
        offset,
    )
}

/// Like `list_articles`, but `sort_by_relevance` controls whether an active
/// search orders by FTS rank (default) or by date only.
pub fn list_articles_sorted(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
    search: Option<&str>,
    oldest_first: bool,
    sort_by_relevance: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    let (mut where_clauses, mut binds) = article_filter(query, unread_only);

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
    // Non-empty input that compiles to nothing → zero rows (spec).
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
                a.is_read, a.is_starred, a.read_later
         FROM articles a JOIN feeds f ON f.id = a.feed_id ",
        snippet_len = PREVIEW_SNIPPET_CHARS,
    );
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
    // Sort by the effective date — COALESCE(published_at, fetched_at) — so
    // an article with no feed-supplied date orders by when it arrived rather
    // than sinking to the bottom. The two columns are stored in different
    // textual formats (`published_at` RFC 3339 with a `T`; `fetched_at` the
    // space form), so a raw string compare mis-orders a list mixing both;
    // `datetime()` normalises each side to a single comparable form. Backed
    // by `idx_articles_sort`, an expression index over the same wrapped
    // expression (v12) — the planner uses it for both directions, no sort.
    //
    // Active search defaults to FTS relevance (`fts.rank`), with date as
    // secondary. Browse (no search) stays chronological. Callers may pass
    // `sort_by_relevance: false` to force date order while searching.
    sql.push_str(" ORDER BY ");
    sql.push_str(&article_order(oldest_first, rank_first));
    sql.push(' ');
    sql.push_str("LIMIT ? OFFSET ?");
    binds.push(Value::Integer(limit));
    binds.push(Value::Integer(offset));

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt
        .query_map(params_from_iter(binds), |r| {
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
                is_read: r.get(10)?,
                is_starred: r.get(11)?,
                read_later: r.get(12)?,
                tags: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    attach_article_tags(conn, &mut rows)?;
    Ok(rows)
}

/// Scan stored articles that have no thumbnail but whose feed or extracted
/// body HTML embeds an image, returning the `(id, image_url)` pairs to adopt.
/// Reads only — paired with `apply_card_images` so the caller can run this
/// heavy parse on a reader connection and the quick writes under the writer
/// lock. One-time: feeds ingested after the body-image fallback shipped
/// already store this at parse.
pub fn card_image_backfill_scan(conn: &Connection) -> AppResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, content_html, extracted_html FROM articles
         WHERE (image_url IS NULL OR trim(image_url) = '')
           AND (
                (content_html IS NOT NULL AND content_html <> '')
                OR (extracted_html IS NOT NULL AND extracted_html <> '')
           )",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, content_html, extracted_html) = row?;
        let img = content_html
            .as_deref()
            .and_then(crate::sanitize::first_image)
            .or_else(|| extracted_html.as_deref().and_then(crate::sanitize::first_image));
        if let Some(img) = img {
            out.push((id, img));
        }
    }
    Ok(out)
}

/// Persist the `(id, image_url)` pairs found by `card_image_backfill_scan`,
/// in a single transaction.
pub fn apply_card_images(conn: &Connection, updates: &[(i64, String)]) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    for (id, img) in updates {
        tx.execute(
            "UPDATE articles SET image_url = ?2 WHERE id = ?1",
            params![id, img],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Turn raw user text into a safe FTS5 MATCH expression.
/// Prefer `crate::search::compile_search` for new call sites.
/// `or_join` maps to recall (OR) vs strict (AND) mode.
/// Expands bare terms via the process wordcloud dict (CN–EN synonyms).
fn fts_query(input: &str, or_join: bool) -> String {
    let mode = if or_join {
        crate::search::SearchMode::Recall
    } else {
        crate::search::SearchMode::Strict
    };
    crate::wordcloud_dict::process_dict().with_dict(|dict| {
        crate::search::fts_match_expr_with_dict(input, mode, Some(dict))
    })
}

/// Retrieve up to `limit` articles relevant to a natural-language `question`,
/// for use as RAG context. Uses OR-joined FTS terms so a multi-word question
/// still matches articles that contain *some* of its keywords — an AND join
/// (as explicit search uses) would require every word to appear and so return
/// nothing for a real question. Returns `(id, title, feed_title)` ordered by
/// FTS relevance. An all-stopword / punctuation-only question yields no rows.
pub fn search_articles_for_rag(
    conn: &Connection,
    question: &str,
    limit: i64,
) -> AppResult<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.title, f.title
         FROM articles a
         JOIN feeds f ON f.id = a.feed_id
         JOIN articles_fts fts ON fts.rowid = a.id
         WHERE articles_fts MATCH ?1
         ORDER BY fts.rank
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![fts_query(question, true), limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Recent articles as `(title, feed_title, text)` for building an AI digest.
pub fn digest_source(conn: &Connection, limit: i64) -> AppResult<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.title, f.title, substr(a.body_text, 1, 600)
         FROM articles a JOIN feeds f ON f.id = a.feed_id
         ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_article(conn: &Connection, id: i64) -> AppResult<ArticleDetail> {
    let mut detail = conn.query_row(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author, a.url,
                a.content_html, a.extracted_html, a.image_url, a.published_at,
                a.is_read, a.is_starred, a.read_later, a.ai_summary,
                a.translated_html, a.translated_lang
         FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE a.id = ?1",
        params![id],
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
                is_read: r.get(11)?,
                is_starred: r.get(12)?,
                read_later: r.get(13)?,
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
    detail.tags = tags_for_article(conn, id)?;
    Ok(detail)
}

/// `(title, plain_text)` for building an AI prompt. Prefers the extracted
/// full text when the user has run extraction, so a summary / answer covers
/// the whole article rather than the (often truncated) feed body.
pub fn article_preview_text(conn: &Connection, id: i64) -> AppResult<(String, String)> {
    conn.query_row(
        "SELECT title, body_text FROM articles WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(Into::into)
}

pub fn article_text(conn: &Connection, id: i64) -> AppResult<(String, String)> {
    let (title, body, extracted): (String, String, Option<String>) = conn.query_row(
        "SELECT title, body_text, extracted_html FROM articles WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let text = match extracted {
        Some(html) if !html.trim().is_empty() => crate::sanitize::html_to_text(&html),
        _ => body,
    };
    Ok((title, text))
}

pub fn set_read(conn: &Connection, id: i64, read: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET is_read = ?2 WHERE id = ?1", params![id, read])?;
    Ok(())
}

pub fn set_starred(conn: &Connection, id: i64, starred: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET is_starred = ?2 WHERE id = ?1", params![id, starred])?;
    Ok(())
}

pub fn set_read_later(conn: &Connection, id: i64, v: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET read_later = ?2 WHERE id = ?1", params![id, v])?;
    Ok(())
}

/// Store the extracted full-text HTML and re-index the article's FTS body
/// with it, so search covers the whole article rather than just the short
/// summary the feed shipped.
pub fn set_extracted_html(
    conn: &Connection,
    id: i64,
    html: &str,
    image_url: Option<&str>,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE articles
            SET extracted_html = ?2,
                image_url = CASE
                    WHEN ?3 IS NOT NULL AND (image_url IS NULL OR trim(image_url) = '')
                    THEN ?3
                    ELSE image_url
                END
         WHERE id = ?1",
        params![id, html, image_url],
    )?;
    tx.execute(
        "UPDATE articles_fts SET body = ?2 WHERE rowid = ?1",
        params![id, crate::sanitize::html_to_text(html)],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_ai_summary(conn: &Connection, id: i64, summary: &str) -> AppResult<()> {
    conn.execute("UPDATE articles SET ai_summary = ?2 WHERE id = ?1", params![id, summary])?;
    Ok(())
}

/// Return a cached list-preview translation for this exact target language and
/// engine, if one exists.
pub fn get_preview_translation(
    conn: &Connection,
    id: i64,
    lang: &str,
    engine: &str,
) -> AppResult<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT title, snippet
               FROM article_preview_translations
              WHERE article_id = ?1 AND lang = ?2 AND engine = ?3",
            params![id, lang, engine],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Cache a list-preview translation independently from full-body translations.
pub fn set_preview_translation(
    conn: &Connection,
    id: i64,
    title: &str,
    snippet: &str,
    lang: &str,
    engine: &str,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO article_preview_translations(article_id, lang, engine, title, snippet)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(article_id, lang, engine) DO UPDATE SET
             title = excluded.title,
             snippet = excluded.snippet",
        params![id, lang, engine, title.trim(), snippet.trim()],
    )?;
    Ok(())
}

pub fn set_translation(conn: &Connection, id: i64, html: &str, lang: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET translated_html = ?2, translated_lang = ?3 WHERE id = ?1",
        params![id, html, lang],
    )?;
    Ok(())
}

/// Mark every article matching the current sidebar selection as read. When
/// `enqueue_sync` is set, the read change is also queued for the sync server
/// — otherwise a bulk mark-all-read never reaches FreshRSS and the next pull
/// silently reverts it.
pub fn mark_all_read(
    conn: &Connection,
    query: &ArticleQuery,
    enqueue_sync: bool,
) -> AppResult<usize> {
    // WHERE fragment selecting the articles in the current view, plus an
    // optional bound id (feed / folder / tag). `pred` is a fixed literal.
    let (pred, id): (&str, Option<i64>) = match query {
        ArticleQuery::All | ArticleQuery::Unread => ("1", None),
        ArticleQuery::Starred => ("is_starred = 1", None),
        ArticleQuery::ReadLater => ("read_later = 1", None),
        ArticleQuery::Feed(id) => ("feed_id = ?1", Some(*id)),
        ArticleQuery::Folder(id) => (
            "feed_id IN (SELECT id FROM feeds WHERE folder_id = ?1)",
            Some(*id),
        ),
        ArticleQuery::Tag(id) => (
            "id IN (SELECT article_id FROM article_tags WHERE tag_id = ?1)",
            Some(*id),
        ),
    };
    let bind: Vec<&dyn rusqlite::ToSql> =
        id.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    // Queue + flip together: the sync-queue rows and the is_read change must
    // commit atomically, or a mid-way failure leaves the queue claiming a
    // read state the articles never reached. Queue *before* flipping so the
    // `is_read = 0` filter still matches; the SELECT's WHERE also
    // disambiguates the ON CONFLICT clause.
    //
    // Articles are queued regardless of whether they already carry a
    // `remote_id`: freshly fetched items have none until a pull matches them
    // by URL, and "mark all read" is most often run right after a refresh on
    // exactly those items. `take_sync_queue` defers any entry whose article
    // still lacks a remote id, so the change pushes on the sync after the id
    // is assigned — mirroring the single-article `enqueue_sync` path. The old
    // `remote_id IS NOT NULL` filter here silently dropped those changes.
    let tx = conn.unchecked_transaction()?;
    if enqueue_sync {
        tx.execute(
            &format!(
                "INSERT INTO sync_queue(article_id, field, value)
                 SELECT id, 'read', 1 FROM articles
                 WHERE {pred} AND is_read = 0
                 ON CONFLICT(article_id, field) DO UPDATE SET value = 1"
            ),
            bind.as_slice(),
        )?;
    }
    let n = tx.execute(
        &format!("UPDATE articles SET is_read = 1 WHERE {pred} AND is_read = 0"),
        bind.as_slice(),
    )?;
    tx.commit()?;
    Ok(n)
}

/// Whether a FreshRSS server is currently linked (a non-empty URL is stored).
pub fn is_freshrss_connected(conn: &Connection) -> bool {
    get_setting(conn, "freshrss_url")
        .ok()
        .flatten()
        .map(|u| !u.trim().is_empty())
        .unwrap_or(false)
}

// ─────────────────────────── tags ───────────────────────────

/// Palette keys cycled through as new tags are created.
const TAG_COLORS: &[&str] = &[
    "clay", "amber", "pine", "teal", "indigo", "violet", "rose", "slate",
];

/// Accept `interest` | `ai`; reject anything else.
pub fn normalize_tag_kind(kind: &str) -> AppResult<&'static str> {
    match kind.trim() {
        TAG_KIND_INTEREST => Ok(TAG_KIND_INTEREST),
        TAG_KIND_AI => Ok(TAG_KIND_AI),
        _ => Err(AppError::code("invalidTagKind")),
    }
}

fn map_tag_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: r.get(0)?,
        name: r.get(1)?,
        color: r.get(2)?,
        position: r.get(3)?,
        kind: r.get(4)?,
        article_count: r.get(5)?,
        // Per-article tag chips pass a literal 0; list endpoints compute live unread.
        unread_count: r.get(6)?,
    })
}

/// Every tag (optionally filtered by kind), ordered for the sidebar, with a
/// live article count and unread ("update") count.
pub fn list_tags(conn: &Connection, kind: Option<&str>) -> AppResult<Vec<Tag>> {
    let kind = match kind {
        Some(k) => Some(normalize_tag_kind(k)?),
        None => None,
    };
    let sql = if kind.is_some() {
        "SELECT t.id, t.name, t.color, t.position, t.kind,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id),
                (SELECT COUNT(*) FROM article_tags at
                   JOIN articles a ON a.id = at.article_id
                  WHERE at.tag_id = t.id AND a.is_read = 0)
         FROM tags t WHERE t.kind = ?1
         ORDER BY t.position, t.name COLLATE NOCASE"
    } else {
        "SELECT t.id, t.name, t.color, t.position, t.kind,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id),
                (SELECT COUNT(*) FROM article_tags at
                   JOIN articles a ON a.id = at.article_id
                  WHERE at.tag_id = t.id AND a.is_read = 0)
         FROM tags t ORDER BY t.position, t.name COLLATE NOCASE"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(k) = kind {
        stmt.query_map(params![k], map_tag_row)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([], map_tag_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

/// Merge `from_id` into `to_id` (same kind): every article tagged
/// `from_id` is re-attached to `to_id` (existing attachments untouched), then
/// `from_id` is deleted (its remaining `article_tags` rows cascade). Returns
/// the number of articles newly attached to the target.
///
/// This is the tool for repairing taxonomy fragmentation — the AI vocabulary
/// accumulated ~33k tags with dozens of near-synonyms for one topic (中东 /
/// Middle East / 中东冲突 / …). An admin merges the variants onto a canonical
/// tag so clicking it surfaces the whole topic.
pub fn merge_tags(conn: &Connection, from_id: i64, to_id: i64) -> AppResult<usize> {
    let tx = conn.unchecked_transaction()?;
    let from_kind: Option<String> = tx
        .query_row("SELECT kind FROM tags WHERE id = ?1", params![from_id], |r| r.get(0))
        .optional()?;
    let to_kind: Option<String> = tx
        .query_row("SELECT kind FROM tags WHERE id = ?1", params![to_id], |r| r.get(0))
        .optional()?;
    if from_id == to_id || from_kind.is_none() || to_kind.is_none() {
        return Err(AppError::code("tagNotFound"));
    }
    if from_kind != to_kind {
        return Err(AppError::code("tagKindMismatch"));
    }
    let moved = tx.execute(
        "INSERT OR IGNORE INTO article_tags(article_id, tag_id)
         SELECT article_id, ?1 FROM article_tags WHERE tag_id = ?2",
        params![to_id, from_id],
    )?;
    tx.execute("DELETE FROM tags WHERE id = ?1", params![from_id])?;
    tx.commit()?;
    Ok(moved)
}

/// Create a tag of the given kind, auto-assigning colour and list position.
///
/// Idempotent on `(kind, name)`: matching is case-insensitive within the kind,
/// so "Rust" and "rust" resolve to one tag. The same display name may exist in
/// both `interest` and `ai` taxonomies as separate rows.
pub fn create_tag(conn: &Connection, name: &str, kind: &str) -> AppResult<i64> {
    let kind = normalize_tag_kind(kind)?;
    // Trim before the dedup lookup and the insert: a name with surrounding
    // whitespace is a distinct string from its trimmed twin, so the
    // `COLLATE NOCASE` lookup would miss the existing tag and spawn a
    // near-duplicate. Normalise at this one chokepoint so the invariant holds
    // regardless of caller-side trimming. Mirrors `create_folder`.
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::code("emptyTagName"));
    }
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND name = ?2 COLLATE NOCASE",
            params![kind, name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    // Position the new tag at the end of its kind. `MAX(position)+1` — not
    // `COUNT(*)` — is required: deleting a tag from the middle leaves a gap,
    // so a fresh `COUNT(*)` would collide with an existing tag's position and
    // the new tag would not sort last (only the name tiebreaker would save
    // it). The colour cycles off the same index so the palette stays varied.
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM tags WHERE kind = ?1",
        params![kind],
        |r| r.get(0),
    )?;
    let color = TAG_COLORS[(next as usize) % TAG_COLORS.len()];
    conn.execute(
        "INSERT INTO tags(name, color, position, kind) VALUES (?1, ?2, ?3, ?4)",
        params![name, color, next, kind],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename a tag, rejecting a name that collides with a *different* existing
/// tag of the same kind.
///
/// Uniqueness is per `(kind, name)`. Match case-insensitively within the kind
/// and return a localisable `tagNameExists` code on clash. Renaming a tag to
/// its own current name (or a case change of it) is allowed.
pub fn rename_tag(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    // Trim so the collision check and the stored value match what `create_tag`
    // would produce — otherwise a rename to a whitespace-padded variant slips
    // past the clash test and recreates the near-duplicate.
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::code("emptyTagName"));
    }
    let kind: String = conn
        .query_row("SELECT kind FROM tags WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| AppError::code("tagNotFound"))?;
    let clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags
             WHERE kind = ?1 AND name = ?2 COLLATE NOCASE AND id != ?3",
            params![kind, name, id],
            |r| r.get(0),
        )
        .optional()?;
    if clash.is_some() {
        return Err(AppError::code("tagNameExists"));
    }
    conn.execute("UPDATE tags SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn set_tag_color(conn: &Connection, id: i64, color: &str) -> AppResult<()> {
    conn.execute("UPDATE tags SET color = ?2 WHERE id = ?1", params![id, color])?;
    Ok(())
}

/// Persist a new tag ordering — `ids` listed in the desired display order.
/// The per-row updates run in one transaction so a mid-loop failure can't
/// leave the tag list in a half-reordered state.
pub fn reorder_tags(conn: &Connection, ids: &[i64]) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    for (pos, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE tags SET position = ?2 WHERE id = ?1",
            params![id, pos as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn delete_tag(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
    Ok(())
}

/// The most frequently used tag names of `kind`, by article count — the
/// "working vocabulary" the auto-tag prompt shows the LLM so it reuses
/// established tags (e.g. "Middle East", 455 articles) instead of inventing a
/// fresh near-synonym per article. Bounded and usage-ranked, so the prompt
/// stays small and stable while the model gets an actionable reuse list.
pub fn top_tag_names(conn: &Connection, kind: &str, limit: i64) -> AppResult<Vec<String>> {
    let kind = normalize_tag_kind(kind)?;
    if limit <= 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT t.name FROM tags t
         WHERE t.kind = ?1
         ORDER BY (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id) DESC,
                  t.name COLLATE NOCASE
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![kind, limit], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── tag aliases ───────────────────────────

fn map_tag_alias_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TagAlias> {
    Ok(TagAlias {
        id: r.get(0)?,
        alias: r.get(1)?,
        tag_id: r.get(2)?,
        kind: r.get(3)?,
        tag_name: r.get(4)?,
    })
}

/// List aliases, optionally filtered by target tag and/or kind.
pub fn list_tag_aliases(
    conn: &Connection,
    tag_id: Option<i64>,
    kind: Option<&str>,
) -> AppResult<Vec<TagAlias>> {
    let kind = match kind {
        Some(k) => Some(normalize_tag_kind(k)?),
        None => None,
    };
    let sql = match (tag_id.is_some(), kind.is_some()) {
        (true, true) => {
            "SELECT a.id, a.alias, a.tag_id, a.kind, t.name
             FROM tag_aliases a JOIN tags t ON t.id = a.tag_id
             WHERE a.tag_id = ?1 AND a.kind = ?2
             ORDER BY a.alias COLLATE NOCASE"
        }
        (true, false) => {
            "SELECT a.id, a.alias, a.tag_id, a.kind, t.name
             FROM tag_aliases a JOIN tags t ON t.id = a.tag_id
             WHERE a.tag_id = ?1
             ORDER BY a.alias COLLATE NOCASE"
        }
        (false, true) => {
            "SELECT a.id, a.alias, a.tag_id, a.kind, t.name
             FROM tag_aliases a JOIN tags t ON t.id = a.tag_id
             WHERE a.kind = ?1
             ORDER BY t.name COLLATE NOCASE, a.alias COLLATE NOCASE"
        }
        (false, false) => {
            "SELECT a.id, a.alias, a.tag_id, a.kind, t.name
             FROM tag_aliases a JOIN tags t ON t.id = a.tag_id
             ORDER BY a.kind, t.name COLLATE NOCASE, a.alias COLLATE NOCASE"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = match (tag_id, kind) {
        (Some(tid), Some(k)) => stmt
            .query_map(params![tid, k], map_tag_alias_row)?
            .collect::<Result<Vec<_>, _>>()?,
        (Some(tid), None) => stmt
            .query_map(params![tid], map_tag_alias_row)?
            .collect::<Result<Vec<_>, _>>()?,
        (None, Some(k)) => stmt
            .query_map(params![k], map_tag_alias_row)?
            .collect::<Result<Vec<_>, _>>()?,
        (None, None) => stmt
            .query_map([], map_tag_alias_row)?
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

/// Resolve an alias string to its canonical tag id within `kind`.
pub fn resolve_tag_alias(conn: &Connection, kind: &str, alias: &str) -> AppResult<Option<i64>> {
    let kind = normalize_tag_kind(kind)?;
    let alias = alias.trim();
    if alias.is_empty() {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT tag_id FROM tag_aliases
             WHERE kind = ?1 AND alias = ?2 COLLATE NOCASE",
            params![kind, alias],
            |r| r.get(0),
        )
        .optional()?)
}

/// Resolve a suggested name to a tag id: exact tag name first, then alias.
///
/// Case-insensitive within `kind`. Does not create tags.
pub fn resolve_tag_by_name_or_alias(
    conn: &Connection,
    kind: &str,
    name: &str,
) -> AppResult<Option<i64>> {
    let kind = normalize_tag_kind(kind)?;
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND name = ?2 COLLATE NOCASE",
            params![kind, name],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(Some(id));
    }
    resolve_tag_alias(conn, kind, name)
}

/// Add an alias for `tag_id`. Kind is taken from the target tag.
///
/// Guardrails: trim; reject empty; reject if the alias equals any tag name of
/// the same kind (case-insensitive); UNIQUE(kind, alias) enforces one mapping.
pub fn create_tag_alias(conn: &Connection, tag_id: i64, alias: &str) -> AppResult<i64> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(AppError::code("emptyTagAlias"));
    }
    let kind: String = conn
        .query_row("SELECT kind FROM tags WHERE id = ?1", params![tag_id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| AppError::code("tagNotFound"))?;
    let kind = normalize_tag_kind(&kind)?;

    let name_clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND name = ?2 COLLATE NOCASE",
            params![kind, alias],
            |r| r.get(0),
        )
        .optional()?;
    if name_clash.is_some() {
        return Err(AppError::code("tagAliasConflictsWithTagName"));
    }

    let alias_clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM tag_aliases WHERE kind = ?1 AND alias = ?2 COLLATE NOCASE",
            params![kind, alias],
            |r| r.get(0),
        )
        .optional()?;
    if alias_clash.is_some() {
        return Err(AppError::code("tagAliasExists"));
    }

    conn.execute(
        "INSERT INTO tag_aliases(alias, tag_id, kind) VALUES (?1, ?2, ?3)",
        params![alias, tag_id, kind],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename an alias; same guardrails as [`create_tag_alias`].
pub fn rename_tag_alias(conn: &Connection, id: i64, alias: &str) -> AppResult<()> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(AppError::code("emptyTagAlias"));
    }
    let kind: String = conn
        .query_row(
            "SELECT kind FROM tag_aliases WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::code("tagAliasNotFound"))?;
    let kind = normalize_tag_kind(&kind)?;

    let name_clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND name = ?2 COLLATE NOCASE",
            params![kind, alias],
            |r| r.get(0),
        )
        .optional()?;
    if name_clash.is_some() {
        return Err(AppError::code("tagAliasConflictsWithTagName"));
    }

    let alias_clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM tag_aliases
             WHERE kind = ?1 AND alias = ?2 COLLATE NOCASE AND id != ?3",
            params![kind, alias, id],
            |r| r.get(0),
        )
        .optional()?;
    if alias_clash.is_some() {
        return Err(AppError::code("tagAliasExists"));
    }

    conn.execute(
        "UPDATE tag_aliases SET alias = ?2 WHERE id = ?1",
        params![id, alias],
    )?;
    Ok(())
}

pub fn delete_tag_alias(conn: &Connection, id: i64) -> AppResult<()> {
    let n = conn.execute("DELETE FROM tag_aliases WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(AppError::code("tagAliasNotFound"));
    }
    Ok(())
}

/// Delete tags of the given kind that have zero `article_tags` rows.
///
/// Only AI tags may be cleaned up this way — interest tags are an
/// admin-maintained closed vocabulary and must be kept even when unused.
/// Returns the number of rows deleted.
pub fn delete_empty_tags(conn: &Connection, kind: &str) -> AppResult<usize> {
    let kind = normalize_tag_kind(kind)?;
    if kind != TAG_KIND_AI {
        return Err(AppError::code("cleanupEmptyInterestForbidden"));
    }
    let n = conn.execute(
        "DELETE FROM tags
         WHERE kind = ?1
           AND NOT EXISTS (
             SELECT 1 FROM article_tags at WHERE at.tag_id = tags.id
           )",
        params![kind],
    )?;
    Ok(n)
}

/// Attach (`on = true`) or detach a tag from one article.
pub fn set_article_tag(conn: &Connection, article_id: i64, tag_id: i64, on: bool) -> AppResult<()> {
    if on {
        conn.execute(
            "INSERT INTO article_tags(article_id, tag_id) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![article_id, tag_id],
        )?;
    } else {
        conn.execute(
            "DELETE FROM article_tags WHERE article_id = ?1 AND tag_id = ?2",
            params![article_id, tag_id],
        )?;
    }
    Ok(())
}

/// How many tags of `kind` are currently attached to an article.
pub fn article_tag_count(conn: &Connection, article_id: i64, kind: &str) -> AppResult<i64> {
    let kind = normalize_tag_kind(kind)?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM article_tags at
         JOIN tags t ON t.id = at.tag_id
         WHERE at.article_id = ?1 AND t.kind = ?2",
        params![article_id, kind],
        |r| r.get(0),
    )?)
}

// ─────────────────────────── auto-tag queue ───────────────────────────

pub const AUTO_TAG_STATUS_PENDING: &str = "pending";
pub const AUTO_TAG_STATUS_PROCESSING: &str = "processing";
pub const AUTO_TAG_STATUS_DONE: &str = "done";
pub const AUTO_TAG_STATUS_FAILED: &str = "failed";

/// Skip re-enqueue of recently completed jobs unless force.
const AUTO_TAG_RECENT_DONE_HOURS: i64 = 24;

/// Enqueue (or re-enqueue) an article for auto-tagging.
///
/// Skips when the row is already `done` within the last 24h (avoids churn).
/// Pending/processing rows are left alone; failed/stale-done rows reset to
/// pending. Returns whether a row was inserted or reset.
pub fn enqueue_auto_tag(conn: &Connection, article_id: i64) -> AppResult<bool> {
    enqueue_auto_tag_inner(conn, article_id, false)
}

/// Force-enqueue (admin backfill): always reset to pending, ignoring the
/// recent-done skip so operators can re-tag on demand.
pub fn enqueue_auto_tag_force(conn: &Connection, article_id: i64) -> AppResult<bool> {
    enqueue_auto_tag_inner(conn, article_id, true)
}

fn enqueue_auto_tag_inner(
    conn: &Connection,
    article_id: i64,
    force: bool,
) -> AppResult<bool> {
    if !force {
        let recent_done: bool = conn
            .query_row(
                "SELECT 1 FROM auto_tag_queue
                 WHERE article_id = ?1
                   AND status = 'done'
                   AND datetime(updated_at) >= datetime('now', ?2)",
                params![article_id, format!("-{AUTO_TAG_RECENT_DONE_HOURS} hours")],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if recent_done {
            return Ok(false);
        }
    }
    // Leave an in-flight claim alone so we do not double-schedule it.
    let in_flight: bool = conn
        .query_row(
            "SELECT 1 FROM auto_tag_queue
             WHERE article_id = ?1 AND status IN ('pending', 'processing')",
            params![article_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if in_flight {
        return Ok(false);
    }
    let n = conn.execute(
        "INSERT INTO auto_tag_queue(article_id, status, attempts, last_error, updated_at)
         VALUES (?1, 'pending', 0, NULL, datetime('now'))
         ON CONFLICT(article_id) DO UPDATE SET
             status = 'pending',
             attempts = 0,
             last_error = NULL,
             updated_at = datetime('now')",
        params![article_id],
    )?;
    Ok(n > 0)
}

/// Enqueue articles for auto-tag backfill.
///
/// - `days > 0`: only articles whose `COALESCE(published_at, fetched_at)` falls
///   within the last `days` days.
/// - `days = 0`: **entire library** (no date filter) — for catching old
///   publish-dated items that sit outside any N-day window.
///
/// Default (`force = false`): never-queued, `failed`, and non-active queue rows
/// whose article still has **zero** `article_tags` become pending. Soft-empty /
/// empty-AI completions stay visible as “untagged” in the UI; re-queuing only
/// those spends tokens where it matters. Articles that already have tags are
/// left alone. Pending/processing rows are never touched.
///
/// With `force = true`: also resets every non-active row (admin re-tag), tags or not.
/// Returns the number of rows newly set to pending.
pub fn enqueue_auto_tag_backfill(
    conn: &Connection,
    days: i64,
    force: bool,
) -> AppResult<usize> {
    let days = days.clamp(0, 365);
    // ON CONFLICT DO UPDATE WHERE … — when the WHERE is false SQLite treats
    // the conflict like IGNORE (no update, not counted in changes()).
    let conflict_filter = if force {
        "auto_tag_queue.status NOT IN ('pending', 'processing')"
    } else {
        // failed always; any non-active row only when the article still has no tags.
        "(auto_tag_queue.status = 'failed'
          OR (auto_tag_queue.status NOT IN ('pending', 'processing')
              AND NOT EXISTS (
                  SELECT 1 FROM article_tags at
                  WHERE at.article_id = auto_tag_queue.article_id
              )))"
    };
    let date_filter = if days == 0 {
        "1 = 1".to_string()
    } else {
        "datetime(COALESCE(a.published_at, a.fetched_at)) >= datetime('now', ?1)".to_string()
    };
    let sql = format!(
        "INSERT INTO auto_tag_queue(article_id, status, attempts, last_error, updated_at)
         SELECT a.id, 'pending', 0, NULL, datetime('now')
         FROM articles a
         WHERE {date_filter}
         ON CONFLICT(article_id) DO UPDATE SET
             status = 'pending',
             attempts = 0,
             last_error = NULL,
             updated_at = datetime('now')
         WHERE {conflict_filter}"
    );
    let n = if days == 0 {
        conn.execute(&sql, [])?
    } else {
        let modifier = format!("-{days} days");
        conn.execute(&sql, params![modifier])?
    };
    Ok(n)
}

/// Article coverage inside the backfill time window (published/fetched).
///
/// `days = 0` means the whole library (same as backfill with no date filter).
/// `untagged` = no rows in `article_tags` (matches what default backfill
/// treats as still needing tags when status is `done`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoTagWindowStats {
    pub days: i64,
    pub articles: i64,
    pub untagged: i64,
    pub tagged: i64,
}

/// Count articles (and untagged subset) in the same window backfill uses.
/// `days = 0` → entire library.
pub fn auto_tag_window_stats(conn: &Connection, days: i64) -> AppResult<AutoTagWindowStats> {
    let days = days.clamp(0, 365);
    let (articles, untagged) = if days == 0 {
        let articles: i64 = conn.query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))?;
        let untagged: i64 = conn.query_row(
            "SELECT COUNT(*) FROM articles a
             WHERE NOT EXISTS (
                 SELECT 1 FROM article_tags at WHERE at.article_id = a.id
             )",
            [],
            |r| r.get(0),
        )?;
        (articles, untagged)
    } else {
        let modifier = format!("-{days} days");
        let articles: i64 = conn.query_row(
            "SELECT COUNT(*) FROM articles a
             WHERE datetime(COALESCE(a.published_at, a.fetched_at))
                   >= datetime('now', ?1)",
            params![modifier],
            |r| r.get(0),
        )?;
        let untagged: i64 = conn.query_row(
            "SELECT COUNT(*) FROM articles a
             WHERE datetime(COALESCE(a.published_at, a.fetched_at))
                   >= datetime('now', ?1)
               AND NOT EXISTS (
                   SELECT 1 FROM article_tags at WHERE at.article_id = a.id
               )",
            params![modifier],
            |r| r.get(0),
        )?;
        (articles, untagged)
    };
    Ok(AutoTagWindowStats {
        days,
        articles,
        untagged,
        tagged: articles - untagged,
    })
}

/// Atomically claim one pending job → `processing`.
///
/// **Ingest-first priority** (so live fetch can cut in while a backlog drains):
/// 1. `fetched_at DESC` — just-ingested articles jump ahead of older-fetched
///    pending jobs, even when their `published_at` is ancient.
/// 2. `published_at DESC` — within the same fetch batch, prefer newer content.
/// 3. `article_id DESC` — stable tie-break.
///
/// We do *not* use `queue.created_at` / `queue.updated_at` — backfill re-enqueue
/// would otherwise let ancient catch-up starve newly collected articles.
///
/// Returns `(article_id, attempts)`, or `None` if the queue is empty / lost a
/// race (another worker claimed the same row).
pub fn claim_auto_tag_job(conn: &Connection) -> AppResult<Option<(i64, i64)>> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT q.article_id, q.attempts
             FROM auto_tag_queue q
             JOIN articles a ON a.id = q.article_id
             WHERE q.status = 'pending'
             ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC,
                      q.article_id DESC
             LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((article_id, attempts)) = row else {
        return Ok(None);
    };
    // CAS: only transition pending → processing so two workers cannot share a job.
    let n = conn.execute(
        "UPDATE auto_tag_queue
         SET status = 'processing', updated_at = datetime('now')
         WHERE article_id = ?1 AND status = 'pending'",
        params![article_id],
    )?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some((article_id, attempts)))
}

/// Return stuck `processing` rows to `pending` (crash / restart recovery).
/// When `older_than_minutes` is `None`, reclaim every processing row.
pub fn reclaim_stale_auto_tag_jobs(
    conn: &Connection,
    older_than_minutes: Option<i64>,
) -> AppResult<usize> {
    let n = if let Some(mins) = older_than_minutes {
        let mins = mins.max(1);
        conn.execute(
            "UPDATE auto_tag_queue
             SET status = 'pending', updated_at = datetime('now')
             WHERE status = 'processing'
               AND datetime(updated_at) <= datetime('now', ?1)",
            params![format!("-{mins} minutes")],
        )?
    } else {
        conn.execute(
            "UPDATE auto_tag_queue
             SET status = 'pending', updated_at = datetime('now')
             WHERE status = 'processing'",
            [],
        )?
    };
    Ok(n)
}

/// Release a claimed job back to pending without burning an attempt
/// (e.g. feature disabled mid-flight).
pub fn release_auto_tag_job(conn: &Connection, article_id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE auto_tag_queue
         SET status = 'pending', updated_at = datetime('now')
         WHERE article_id = ?1 AND status = 'processing'",
        params![article_id],
    )?;
    Ok(())
}

pub fn mark_auto_tag_done(conn: &Connection, article_id: i64) -> AppResult<()> {
    // Upsert so interactive / sync tagging (never queued) still lands as done.
    conn.execute(
        "INSERT INTO auto_tag_queue(article_id, status, attempts, last_error, updated_at)
         VALUES (?1, 'done', 0, NULL, datetime('now'))
         ON CONFLICT(article_id) DO UPDATE SET
             status = 'done',
             last_error = NULL,
             updated_at = datetime('now')",
        params![article_id],
    )?;
    Ok(())
}

/// Record a failure. If `attempts + 1` reaches `max_attempts`, mark failed;
/// otherwise bump attempts and leave pending for retry.
pub fn mark_auto_tag_failure(
    conn: &Connection,
    article_id: i64,
    error: &str,
    max_attempts: i64,
) -> AppResult<()> {
    let attempts: i64 = conn
        .query_row(
            "SELECT attempts FROM auto_tag_queue WHERE article_id = ?1",
            params![article_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let next = attempts + 1;
    let status = if next >= max_attempts {
        AUTO_TAG_STATUS_FAILED
    } else {
        AUTO_TAG_STATUS_PENDING
    };
    // Truncate error so a huge provider body cannot bloat the row.
    let err: String = error.chars().take(500).collect();
    conn.execute(
        "UPDATE auto_tag_queue
         SET status = ?2, attempts = ?3, last_error = ?4, updated_at = datetime('now')
         WHERE article_id = ?1",
        params![article_id, status, next, err],
    )?;
    Ok(())
}

/// True when every enabled taxonomy is already at its per-article cap
/// (nothing useful left for the LLM to add).
pub fn article_at_auto_tag_caps(
    conn: &Connection,
    article_id: i64,
    interest_on: bool,
    ai_on: bool,
    interest_max: i64,
    ai_max: i64,
) -> AppResult<bool> {
    if !interest_on && !ai_on {
        return Ok(true);
    }
    if interest_on {
        let n = article_tag_count(conn, article_id, crate::models::TAG_KIND_INTEREST)?;
        if n < interest_max {
            return Ok(false);
        }
    }
    if ai_on {
        let n = article_tag_count(conn, article_id, crate::models::TAG_KIND_AI)?;
        if n < ai_max {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoTagQueueStatus {
    pub pending: i64,
    pub processing: i64,
    pub failed: i64,
    pub done: i64,
    pub last_error: Option<String>,
}

pub fn auto_tag_queue_status(conn: &Connection) -> AppResult<AutoTagQueueStatus> {
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM auto_tag_queue WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    let processing: i64 = conn.query_row(
        "SELECT COUNT(*) FROM auto_tag_queue WHERE status = 'processing'",
        [],
        |r| r.get(0),
    )?;
    let failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM auto_tag_queue WHERE status = 'failed'",
        [],
        |r| r.get(0),
    )?;
    let done: i64 = conn.query_row(
        "SELECT COUNT(*) FROM auto_tag_queue WHERE status = 'done'",
        [],
        |r| r.get(0),
    )?;
    let last_error: Option<String> = conn
        .query_row(
            "SELECT last_error FROM auto_tag_queue
             WHERE last_error IS NOT NULL AND last_error != ''
             ORDER BY updated_at DESC
             LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(AutoTagQueueStatus {
        pending,
        processing,
        failed,
        done,
        last_error,
    })
}

/// Soft pause / restart: delete backlog and in-flight work, keep `done` history.
///
/// Removes rows with status in (`pending`, `processing`, `failed`). Keeping
/// `done` preserves tagged history so a later default backfill does not
/// needlessly re-enqueue every already-tagged article (0-tag `done` can still
/// be re-queued by backfill). In-flight workers that finish after a clear will
/// no-op on mark-done/failure if the row is gone.
///
/// Does **not** enqueue anything — the admin must run backfill manually.
/// Returns the number of rows deleted.
pub fn clear_auto_tag_queue(conn: &Connection) -> AppResult<usize> {
    let n = conn.execute(
        "DELETE FROM auto_tag_queue
         WHERE status IN ('pending', 'processing', 'failed')",
        [],
    )?;
    Ok(n)
}

/// Tags attached to one article (counts left at 0 — unused per-article).
pub fn tags_for_article(conn: &Connection, article_id: i64) -> AppResult<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color, t.position, t.kind, 0, 0
         FROM tags t JOIN article_tags at ON at.tag_id = t.id
         WHERE at.article_id = ?1
         ORDER BY t.kind, t.position, t.name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![article_id], map_tag_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Batch-load tags for many articles in one query. Used by list endpoints so
/// the middle-column article list can render chips without N+1 round-trips.
pub fn tags_for_articles(
    conn: &Connection,
    article_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, Vec<Tag>>> {
    let mut out: std::collections::HashMap<i64, Vec<Tag>> =
        std::collections::HashMap::with_capacity(article_ids.len());
    if article_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = article_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT at.article_id, t.id, t.name, t.color, t.position, t.kind, 0, 0
         FROM tags t JOIN article_tags at ON at.tag_id = t.id
         WHERE at.article_id IN ({placeholders})
         ORDER BY at.article_id, t.kind, t.position, t.name COLLATE NOCASE"
    );
    let binds: Vec<Value> = article_ids.iter().copied().map(Value::Integer).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds), |r| {
        let article_id: i64 = r.get(0)?;
        let tag = Tag {
            id: r.get(1)?,
            name: r.get(2)?,
            color: r.get(3)?,
            position: r.get(4)?,
            kind: r.get(5)?,
            article_count: r.get(6)?,
            unread_count: r.get(7)?,
        };
        Ok((article_id, tag))
    })?;
    for row in rows {
        let (article_id, tag) = row?;
        out.entry(article_id).or_default().push(tag);
    }
    Ok(out)
}

/// Fill `ArticleSummary.tags` for a page of list rows (one batch query).
pub fn attach_article_tags(conn: &Connection, rows: &mut [ArticleSummary]) -> AppResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = rows.iter().map(|a| a.id).collect();
    let mut by_id = tags_for_articles(conn, &ids)?;
    for a in rows.iter_mut() {
        a.tags = by_id.remove(&a.id).unwrap_or_default();
    }
    Ok(())
}

// ─────────────────────────── filter rules ───────────────────────────

fn row_to_rule(r: &rusqlite::Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        name: r.get(1)?,
        enabled: r.get(2)?,
        feed_id: r.get(3)?,
        field: r.get(4)?,
        query: r.get(5)?,
        action: r.get(6)?,
        position: r.get(7)?,
    })
}

const RULE_COLS: &str = "id, name, enabled, feed_id, field, query, action, position";

/// Every rule, enabled or not, ordered for the settings list.
pub fn list_rules(conn: &Connection) -> AppResult<Vec<Rule>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {RULE_COLS} FROM rules ORDER BY position, id"))?;
    let rows = stmt
        .query_map([], row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Only the enabled rules — the set evaluated against incoming articles.
pub fn active_rules(conn: &Connection) -> AppResult<Vec<Rule>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RULE_COLS} FROM rules WHERE enabled = 1 ORDER BY position, id"
    ))?;
    let rows = stmt
        .query_map([], row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_rule(
    conn: &Connection,
    name: &str,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<i64> {
    // Position the new rule at the end. `MAX(position)+1` — not `COUNT(*)` —
    // is required: deleting a rule from the middle leaves a gap, so a fresh
    // `COUNT(*)` would collide with an existing rule's position and the new
    // rule would not sort last (`ORDER BY position, id` would then slot it
    // before any later-positioned rule). Rule order is semantically load-
    // bearing — `active_rules` evaluates in this order and a `skip` match
    // short-circuits — so a stale position can change which action fires.
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM rules",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO rules(name, feed_id, field, query, action, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, feed_id, field, query, action, next],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn update_rule(
    conn: &Connection,
    id: i64,
    name: &str,
    enabled: bool,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<()> {
    conn.execute(
        "UPDATE rules SET name = ?2, enabled = ?3, feed_id = ?4,
                          field = ?5, query = ?6, action = ?7
         WHERE id = ?1",
        params![id, name, enabled, feed_id, field, query, action],
    )?;
    Ok(())
}

pub fn delete_rule(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM rules WHERE id = ?1", params![id])?;
    Ok(())
}

/// Build the `WHERE` fragment (and its bind values) that selects the articles a
/// rule matches: one `unicode_lower(col) LIKE ?` per (keyword × searched
/// column), OR-joined, optionally scoped to one feed. Columns are *unaliased*
/// so the fragment slots straight into a bare `SELECT … FROM articles`,
/// `UPDATE articles` or `DELETE FROM articles`. Returns `None` when the query
/// holds no usable keywords (a no-op rule), so callers can short-circuit.
///
/// `preview_rule` (count + samples) and `apply_rule_to_existing` (act) share
/// this builder so the number the preview shows is exactly the set the apply
/// touches. LIKE wildcards in a keyword are escaped so a literal `%` / `_`
/// can't widen the match; the column side is folded with `unicode_lower` (not
/// SQLite's ASCII-only `LOWER`) so it matches the Unicode-aware
/// `to_lowercase()` `rule_matches` applies at ingestion — otherwise a keyword
/// like `café` would be counted here but its `CAFÉ` articles missed, diverging
/// the preview/apply from live ingestion.
fn rule_match_where(
    field: &str,
    query: &str,
    feed_id: Option<i64>,
) -> Option<(String, Vec<Value>)> {
    let terms: Vec<String> = query
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return None;
    }
    let cols: &[&str] = match field {
        "author" => &["author"],
        "content" => &["body_text"],
        "any" => &["title", "author", "body_text"],
        _ => &["title"],
    };
    let mut ors: Vec<String> = Vec::new();
    let mut binds: Vec<Value> = Vec::new();
    for term in &terms {
        let escaped = term
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        for col in cols {
            ors.push(format!("unicode_lower(COALESCE({col},'')) LIKE ? ESCAPE '\\'"));
            binds.push(Value::Text(format!("%{escaped}%")));
        }
    }
    let mut where_sql = format!("({})", ors.join(" OR "));
    if let Some(fid) = feed_id {
        where_sql.push_str(" AND feed_id = ?");
        binds.push(Value::Integer(fid));
    }
    Some((where_sql, binds))
}

/// Preview how a draft rule would behave: the number of *already-stored*
/// articles its keywords match, plus a handful of recent sample titles.
/// Lets the user sanity-check a rule before saving it.
pub fn preview_rule(
    conn: &Connection,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
) -> AppResult<(i64, Vec<String>)> {
    let Some((where_sql, binds)) = rule_match_where(field, query, feed_id) else {
        return Ok((0, Vec::new()));
    };
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM articles WHERE {where_sql}"),
        params_from_iter(binds.iter().cloned()),
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT title FROM articles WHERE {where_sql}
         ORDER BY datetime(COALESCE(published_at, fetched_at)) DESC, id DESC
         LIMIT 5"
    ))?;
    let samples = stmt
        .query_map(params_from_iter(binds), |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((count, samples))
}

/// Apply a saved rule's action to the articles already in the store that it
/// matches — the one-time backfill run when a rule is created or edited so it
/// affects the existing backlog, not only articles fetched afterwards. Returns
/// the number of articles acted on.
///
/// `skip` *deletes* its matches — the stored-article equivalent of dropping the
/// article at ingestion. FK `ON DELETE CASCADE` (enclosures, highlights, tags)
/// and the `articles_fts_ad` trigger keep dependent rows and the FTS index in
/// sync, the same path retention cleanup relies on. `read` / `star` set the
/// matching flag and skip rows that already carry it, so the returned count is
/// the number of rows actually changed.
pub fn apply_rule_to_existing(
    conn: &Connection,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<usize> {
    let Some((where_sql, binds)) = rule_match_where(field, query, feed_id) else {
        return Ok(0);
    };
    let sql = match action {
        "skip" => format!("DELETE FROM articles WHERE {where_sql}"),
        "read" => {
            format!("UPDATE articles SET is_read = 1 WHERE ({where_sql}) AND is_read = 0")
        }
        "star" => {
            format!("UPDATE articles SET is_starred = 1 WHERE ({where_sql}) AND is_starred = 0")
        }
        _ => return Ok(0),
    };
    Ok(conn.execute(&sql, params_from_iter(binds))?)
}

/// (total unread, starred, read-later) counts for the sidebar smart folders.
pub fn smart_counts(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    Ok(conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM articles WHERE is_read = 0),
            (SELECT COUNT(*) FROM articles WHERE is_starred = 1),
            (SELECT COUNT(*) FROM articles WHERE read_later = 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

// ─────────────────────────── highlights ───────────────────────────

const HIGHLIGHT_COLS: &str =
    "id, article_id, quote, prefix, suffix, text_offset, color, note, created_at";

fn row_to_highlight(r: &rusqlite::Row) -> rusqlite::Result<Highlight> {
    Ok(Highlight {
        id: r.get(0)?,
        article_id: r.get(1)?,
        quote: r.get(2)?,
        prefix: r.get(3)?,
        suffix: r.get(4)?,
        text_offset: r.get(5)?,
        color: r.get(6)?,
        note: r.get(7)?,
        created_at: r.get(8)?,
    })
}

/// The fields needed to create a highlight — everything in [`Highlight`]
/// except the database-assigned `id` and `created_at`. Grouping the anchor
/// fields (which are all `&str` and otherwise trivially swappable) into one
/// named value keeps `insert_highlight` calls unambiguous.
pub struct NewHighlight<'a> {
    pub article_id: i64,
    pub quote: &'a str,
    pub prefix: &'a str,
    pub suffix: &'a str,
    pub text_offset: i64,
    pub color: &'a str,
    pub note: &'a str,
}

/// Insert a highlight and return its new id.
pub fn insert_highlight(conn: &Connection, h: &NewHighlight) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO highlights(article_id, quote, prefix, suffix, text_offset, color, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            h.article_id,
            h.quote,
            h.prefix,
            h.suffix,
            h.text_offset,
            h.color,
            h.note
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// All highlights for one article, oldest first (their reading order).
pub fn list_highlights(conn: &Connection, article_id: i64) -> AppResult<Vec<Highlight>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HIGHLIGHT_COLS} FROM highlights
         WHERE article_id = ?1 ORDER BY text_offset, id"
    ))?;
    let rows = stmt
        .query_map(params![article_id], row_to_highlight)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every highlight across all articles — used by the Highlights browser.
pub fn list_all_highlights(conn: &Connection) -> AppResult<Vec<Highlight>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HIGHLIGHT_COLS} FROM highlights ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = stmt
        .query_map([], row_to_highlight)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Fetch one highlight by id, if it exists.
#[allow(dead_code)] // exercised by the db tests; kept as a complete CRUD API.
pub fn get_highlight(conn: &Connection, id: i64) -> AppResult<Option<Highlight>> {
    Ok(conn
        .query_row(
            &format!("SELECT {HIGHLIGHT_COLS} FROM highlights WHERE id = ?1"),
            params![id],
            row_to_highlight,
        )
        .optional()?)
}

/// Replace a highlight's note text (an empty string clears it).
pub fn update_highlight_note(conn: &Connection, id: i64, note: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE highlights SET note = ?2 WHERE id = ?1",
        params![id, note],
    )?;
    Ok(())
}

/// Change a highlight's colour (a palette key).
pub fn set_highlight_color(conn: &Connection, id: i64, color: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE highlights SET color = ?2 WHERE id = ?1",
        params![id, color],
    )?;
    Ok(())
}

pub fn delete_highlight(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM highlights WHERE id = ?1", params![id])?;
    Ok(())
}

// ─────────────────────────── settings ───────────────────────────

pub fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Read a setting and parse it as `T`, falling back to `default` when the key
/// is missing, unreadable, or fails to parse.
pub fn setting_parsed<T: std::str::FromStr>(conn: &Connection, key: &str, default: T) -> T {
    get_setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read a setting as a boolean flag — `"1"` and `"true"` are true, anything
/// else (including a missing key) falls back to `default`.
pub fn setting_flag(conn: &Connection, key: &str, default: bool) -> bool {
    get_setting(conn, key)
        .ok()
        .flatten()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(default)
}

// ─────────────────────────── storage ───────────────────────────

/// `(database bytes, article count, feed count)` for the storage panel.
pub fn storage_stats(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let articles: i64 = conn.query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))?;
    let feeds: i64 = conn.query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0))?;
    Ok((page_count * page_size, articles, feeds))
}

// ─────────────────────────── ai usage ───────────────────────────

/// Official DeepSeek V4 Flash prices (CNY per million tokens).
/// Source: <https://api-docs.deepseek.com/zh-cn/quick_start/pricing/>
pub const DEFAULT_AI_PRICE_CACHE_HIT_PER_M: f64 = 0.02;
pub const DEFAULT_AI_PRICE_CACHE_MISS_PER_M: f64 = 1.0;
pub const DEFAULT_AI_PRICE_OUTPUT_PER_M: f64 = 2.0;

/// Setting keys for per-million-token prices (CNY). Defaults match
/// `deepseek-v4-flash` official pricing.
pub const SETTING_AI_PRICE_CACHE_HIT: &str = "ai_price_cache_hit_per_m";
pub const SETTING_AI_PRICE_CACHE_MISS: &str = "ai_price_input_per_m";
pub const SETTING_AI_PRICE_OUTPUT: &str = "ai_price_output_per_m";

/// One aggregate bucket of AI token usage.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageRow {
    /// `"total"` for the grand totals; otherwise the feature name
    /// (`summarize`, `ask`, `digest`, `translate`, `translate-preview`,
    /// `auto-tag`).
    pub feature: String,
    /// Number of completed AI calls in this bucket.
    pub calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Portion of `completion_tokens` spent on chain-of-thought reasoning.
    pub reasoning_tokens: i64,
    /// Input tokens billed at the cache-hit rate.
    pub cache_hit_tokens: i64,
}

/// AI usage aggregated over a trailing window.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageStats {
    pub total: AiUsageRow,
    pub by_feature: Vec<AiUsageRow>,
}

/// Persist one completed AI call. Rows that report no tokens at all (a
/// machine-translation engine, or a provider that never surfaces usage) are
/// skipped so the table stays a meaningful LLM ledger.
pub fn record_ai_usage(
    conn: &Connection,
    feature: &str,
    provider: &str,
    model: &str,
    usage: crate::ai::TokenUsage,
) -> AppResult<()> {
    if usage.is_empty() {
        return Ok(());
    }
    // Clamp cache hits to prompt size so a buggy provider can't inflate the
    // cheap bucket past total input.
    let cache_hit = usage.cache_hit_tokens.min(usage.prompt_tokens);
    conn.execute(
        "INSERT INTO ai_usage
             (feature, provider, model, prompt_tokens, completion_tokens,
              reasoning_tokens, cache_hit_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            feature,
            provider,
            model,
            usage.prompt_tokens as i64,
            usage.completion_tokens as i64,
            usage.reasoning_tokens as i64,
            cache_hit as i64,
        ],
    )?;
    Ok(())
}

/// Number of completed LLM calls for `feature` since the start of today (UTC).
/// Used by the auto-tag workers to enforce the daily call budget
/// (`ai_tag_daily_budget`), so a content spike can never surprise-bill the
/// account — the queue simply waits until the next day.
pub fn count_ai_usage_today(conn: &Connection, feature: &str) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM ai_usage
         WHERE feature = ?1 AND created_at >= date('now')",
        params![feature],
        |r| r.get(0),
    )?)
}

/// TTL for the AI digest cache (seconds). The digest feeds ~30 articles to the
/// model, so regenerating it on every click burns ~5k prompt + 4k output
/// tokens for a briefing the user just saw. A pure TTL (no article-level
/// invalidation) is the right trade: the briefing is a snapshot, and new
/// articles arrive constantly — invalidating on each one would defeat the
/// cache.
pub const DIGEST_CACHE_TTL_SECS: i64 = 3600;

/// The cached AI digest, if one exists and is still fresh. Stored under
/// ordinary settings keys so no schema change is needed.
pub fn get_digest_cache(conn: &Connection) -> AppResult<Option<String>> {
    let Some(at) = get_setting(conn, "digest_cache_at")? else {
        return Ok(None);
    };
    let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&at, "%Y-%m-%d %H:%M:%S") else {
        return Ok(None);
    };
    let age = chrono::Utc::now()
        .naive_utc()
        .signed_duration_since(naive)
        .num_seconds();
    if !(0..=DIGEST_CACHE_TTL_SECS).contains(&age) {
        return Ok(None);
    }
    let text = get_setting(conn, "digest_cache_text")?;
    match text {
        Some(t) if !t.trim().is_empty() => Ok(Some(t)),
        _ => Ok(None),
    }
}

/// Store a freshly generated digest (callers persist only completed, non-empty
/// text so a truncated fragment is never cached).
pub fn set_digest_cache(conn: &Connection, text: &str) -> AppResult<()> {
    set_setting(conn, "digest_cache_text", text)?;
    set_setting(
        conn,
        "digest_cache_at",
        &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

// ─────────────────────────── official balance / usage ───────────────────────────

/// One day of the official DeepSeek balance ledger, with the spend derived
/// from consecutive snapshots.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BalanceDay {
    pub day: String,
    pub total_balance: f64,
    pub granted_balance: f64,
    pub topped_up_balance: f64,
    /// Money spent that day (previous total − this total). `None` on the
    /// first recorded day (no baseline).
    pub spend: Option<f64>,
    /// Top-up detected that day (total rose vs the previous snapshot).
    pub topup: Option<f64>,
}

/// Upsert today's balance snapshot (one row per UTC day).
pub fn record_balance_snapshot(
    conn: &Connection,
    total: f64,
    granted: f64,
    topped_up: f64,
) -> AppResult<()> {
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    conn.execute(
        "INSERT INTO ai_balance_history(recorded_at, total_balance, granted_balance, topped_up_balance)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(recorded_at) DO UPDATE SET
             total_balance = excluded.total_balance,
             granted_balance = excluded.granted_balance,
             topped_up_balance = excluded.topped_up_balance",
        params![day, total, granted, topped_up],
    )?;
    Ok(())
}

/// The most recent balance snapshot, if any.
pub fn latest_balance(conn: &Connection) -> AppResult<Option<BalanceDay>> {
    Ok(conn
        .query_row(
            "SELECT recorded_at, total_balance, granted_balance, topped_up_balance
             FROM ai_balance_history ORDER BY recorded_at DESC LIMIT 1",
            [],
            |r| {
                Ok(BalanceDay {
                    day: r.get(0)?,
                    total_balance: r.get(1)?,
                    granted_balance: r.get(2)?,
                    topped_up_balance: r.get(3)?,
                    spend: None,
                    topup: None,
                })
            },
        )
        .optional()?)
}

/// The most recent snapshot day (`YYYY-MM-DD`), for the daily job's
/// once-per-day gate. `None` when nothing has ever been recorded.
pub fn last_balance_day(conn: &Connection) -> AppResult<Option<String>> {
    Ok(conn.query_row(
        "SELECT MAX(recorded_at) FROM ai_balance_history",
        [],
        |r| r.get::<_, Option<String>>(0),
    )?)
}

/// The trailing `days` of balance history (oldest first), with per-day spend
/// derived from consecutive totals — the *real* money the account consumed,
/// straight from the official `/user/balance` endpoint.
pub fn balance_history(conn: &Connection, days: i64) -> AppResult<Vec<BalanceDay>> {
    let days = days.clamp(1, 366);
    let mut stmt = conn.prepare(
        "SELECT recorded_at, total_balance, granted_balance, topped_up_balance
         FROM ai_balance_history
         ORDER BY recorded_at DESC
         LIMIT ?1",
    )?;
    let mut rows: Vec<BalanceDay> = stmt
        .query_map(params![days], |r| {
            Ok(BalanceDay {
                day: r.get(0)?,
                total_balance: r.get(1)?,
                granted_balance: r.get(2)?,
                topped_up_balance: r.get(3)?,
                spend: None,
                topup: None,
            })
        })?
        .collect::<Result<_, _>>()?;
    // Newest first → walk backward to attach the previous day's total.
    for i in 0..rows.len() {
        if let Some(prev) = rows.get(i + 1) {
            let delta = rows[i].total_balance - prev.total_balance;
            if delta < -0.0005 {
                rows[i].spend = Some(-delta);
                rows[i].topup = None;
            } else if delta > 0.0005 {
                rows[i].spend = None;
                rows[i].topup = Some(delta);
            } else {
                rows[i].spend = Some(0.0);
                rows[i].topup = None;
            }
        }
    }
    rows.reverse();
    Ok(rows)
}

/// One day of official dashboard usage (tokens + cost), from the platform
/// endpoints — best-effort and only present when a platform token is set.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OfficialUsageDay {
    pub day: String,
    pub tokens: i64,
    pub cost: f64,
}

pub fn upsert_official_usage(conn: &Connection, day: &str, tokens: i64, cost: f64) -> AppResult<()> {
    conn.execute(
        "INSERT INTO ai_official_usage(day, tokens, cost) VALUES (?1, ?2, ?3)
         ON CONFLICT(day) DO UPDATE SET tokens = excluded.tokens, cost = excluded.cost",
        params![day, tokens, cost],
    )?;
    Ok(())
}

pub fn official_usage(conn: &Connection, days: i64) -> AppResult<Vec<OfficialUsageDay>> {
    let days = days.clamp(1, 366);
    let mut stmt = conn.prepare(
        "SELECT day, tokens, cost FROM ai_official_usage
         ORDER BY day DESC LIMIT ?1",
    )?;
    let mut rows = stmt
        .query_map(params![days], |r| {
            Ok(OfficialUsageDay {
                day: r.get(0)?,
                tokens: r.get(1)?,
                cost: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.reverse();
    Ok(rows)
}

/// Aggregate AI usage over the trailing `days` (clamped to 1–366), bucketed by
/// feature, with grand totals in `total`.
pub fn ai_usage_stats(conn: &Connection, days: i64) -> AppResult<AiUsageStats> {
    let days = days.clamp(1, 366);
    let since = format!("-{days} days");
    let mut total = AiUsageRow {
        feature: "total".into(),
        calls: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        reasoning_tokens: 0,
        cache_hit_tokens: 0,
    };
    let mut by_feature = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT feature, COUNT(*), SUM(prompt_tokens), SUM(completion_tokens),
                SUM(reasoning_tokens), SUM(cache_hit_tokens)
         FROM ai_usage
         WHERE created_at >= datetime('now', ?1)
         GROUP BY feature
         ORDER BY feature",
    )?;
    let rows = stmt.query_map(params![since], |r| {
        Ok(AiUsageRow {
            feature: r.get(0)?,
            calls: r.get(1)?,
            prompt_tokens: r.get(2)?,
            completion_tokens: r.get(3)?,
            reasoning_tokens: r.get(4)?,
            cache_hit_tokens: r.get(5)?,
        })
    })?;
    for row in rows {
        let row = row?;
        total.calls += row.calls;
        total.prompt_tokens += row.prompt_tokens;
        total.completion_tokens += row.completion_tokens;
        total.reasoning_tokens += row.reasoning_tokens;
        total.cache_hit_tokens += row.cache_hit_tokens;
        by_feature.push(row);
    }
    Ok(AiUsageStats { total, by_feature })
}

/// Estimate CNY cost from aggregated usage using DeepSeek-style pricing:
/// `cache_hit × hit + (prompt − cache_hit) × miss + completion × output`.
/// Reasoning tokens are included in `completion_tokens` and share the output
/// rate. Defaults are official `deepseek-v4-flash` yuan prices.
pub fn estimate_ai_cost_cny(conn: &Connection, stats: &AiUsageStats) -> f64 {
    let hit_per_m: f64 =
        setting_parsed(conn, SETTING_AI_PRICE_CACHE_HIT, DEFAULT_AI_PRICE_CACHE_HIT_PER_M);
    let miss_per_m: f64 =
        setting_parsed(conn, SETTING_AI_PRICE_CACHE_MISS, DEFAULT_AI_PRICE_CACHE_MISS_PER_M);
    let out_per_m: f64 =
        setting_parsed(conn, SETTING_AI_PRICE_OUTPUT, DEFAULT_AI_PRICE_OUTPUT_PER_M);
    let t = &stats.total;
    let cache_hit = t.cache_hit_tokens.max(0) as f64;
    let cache_miss = (t.prompt_tokens - t.cache_hit_tokens).max(0) as f64;
    let completion = t.completion_tokens.max(0) as f64;
    cache_hit / 1e6 * hit_per_m + cache_miss / 1e6 * miss_per_m + completion / 1e6 * out_per_m
}

/// Daily article counts for the last `days` calendar days (inclusive of today).
///
/// Counts by `fetched_at` — when the article was ingested into the database
/// (sidebar label 收录 / "collected") — not by `published_at`. An OPML import
/// or first refresh of a long archive would otherwise under-count: many rows
/// have old publish dates but were only just 收录'd.
///
/// Calendar days use the server's local timezone (`localtime`) so an
/// Asia/Shanghai host buckets by CST rather than UTC midnight.
/// Returns `(YYYY-MM-DD, count)` rows — days with zero articles are omitted.
/// Per-day article ingest counts (`fetched_at`, local calendar), newest day first.
pub fn daily_article_counts(conn: &Connection, days: i64) -> AppResult<Vec<(String, i64)>> {
    let days = days.clamp(1, 366);
    // Inclusive window: today and the preceding (days − 1) calendar days.
    let offset = -(days - 1);
    let modifier = format!("{offset} days");
    let mut stmt = conn.prepare(
        "SELECT date(datetime(fetched_at, 'localtime')) AS d,
                COUNT(*) AS c
         FROM articles
         WHERE datetime(fetched_at, 'localtime')
               >= datetime('now', 'localtime', 'start of day', ?1)
         GROUP BY d
         ORDER BY d DESC",
    )?;
    let rows = stmt
        .query_map(params![modifier], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Like [`daily_article_counts`], but every calendar day in the window is
/// present (zero-filled) so charts/lists do not have gaps.
/// Days are newest-first (today at index 0).
pub fn daily_article_counts_filled(
    conn: &Connection,
    days: i64,
) -> AppResult<Vec<(String, i64)>> {
    use chrono::{Duration, Local};
    let days = days.clamp(1, 366);
    let sparse = daily_article_counts(conn, days)?;
    let map: std::collections::HashMap<String, i64> = sparse.into_iter().collect();
    let today = Local::now().date_naive();
    let mut out = Vec::with_capacity(days as usize);
    for i in 0..days {
        let d = today - Duration::days(i);
        let key = d.format("%Y-%m-%d").to_string();
        let count = map.get(&key).copied().unwrap_or(0);
        out.push((key, count));
    }
    Ok(out)
}

/// Counts of articles that carry at least one tag, plus interest/AI breakdowns.
/// An article with both kinds is counted in `tagged`, `tagged_interest`, and
/// `tagged_ai` (breakdowns are not mutually exclusive).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaggedArticleCounts {
    pub tagged: i64,
    pub tagged_interest: i64,
    pub tagged_ai: i64,
}

pub fn tagged_article_counts(conn: &Connection) -> AppResult<TaggedArticleCounts> {
    let tagged: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT article_id) FROM article_tags",
        [],
        |r| r.get(0),
    )?;
    let tagged_interest: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT at.article_id)
         FROM article_tags at
         JOIN tags t ON t.id = at.tag_id
         WHERE t.kind = ?1",
        params![crate::models::TAG_KIND_INTEREST],
        |r| r.get(0),
    )?;
    let tagged_ai: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT at.article_id)
         FROM article_tags at
         JOIN tags t ON t.id = at.tag_id
         WHERE t.kind = ?1",
        params![crate::models::TAG_KIND_AI],
        |r| r.get(0),
    )?;
    Ok(TaggedArticleCounts {
        tagged,
        tagged_interest,
        tagged_ai,
    })
}

/// Admin dashboard snapshot: totals, tagging coverage, queue, daily ingest.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsOverview {
    pub total_articles: i64,
    pub feeds: i64,
    pub tagged_articles: i64,
    pub tagged_interest: i64,
    pub tagged_ai: i64,
    pub queue: AutoTagQueueStatus,
    pub daily: Vec<DailyCount>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyCount {
    pub date: String,
    pub count: i64,
}

/// Build the admin stats overview. `daily_days` defaults to 30 when ≤ 0.
pub fn stats_overview(conn: &Connection, daily_days: i64) -> AppResult<StatsOverview> {
    let daily_days = if daily_days <= 0 { 30 } else { daily_days };
    let (_, total_articles, feeds) = storage_stats(conn)?;
    let tagged = tagged_article_counts(conn)?;
    let queue = auto_tag_queue_status(conn)?;
    let daily = daily_article_counts_filled(conn, daily_days)?
        .into_iter()
        .map(|(date, count)| DailyCount { date, count })
        .collect();
    Ok(StatsOverview {
        total_articles,
        feeds,
        tagged_articles: tagged.tagged,
        tagged_interest: tagged.tagged_interest,
        tagged_ai: tagged.tagged_ai,
        queue,
        daily,
    })
}

/// Delete read articles older than `days`, keeping starred / read-later ones.
/// Returns the number removed. Age is the effective date —
/// COALESCE(published_at, fetched_at) — so a dateless article is retained by
/// fetch age rather than living forever (fetched_at is never NULL).
///
/// The two timestamp columns are stored in different textual formats:
/// `published_at` is RFC 3339 (`2024-01-15T10:30:00+00:00`, written by
/// `to_rfc3339`) while `fetched_at` and `datetime('now', …)` use SQLite's
/// space-separated form (`2024-01-15 10:30:00`). A raw string `<` mis-orders
/// them — the `T` byte sorts *after* a space, so a `published_at` value looks
/// almost a day newer than it is and same-day articles escape the cutoff.
/// Wrapping every side in `datetime()` parses both formats to the canonical
/// representation, so the comparison reflects the real instant.
pub fn cleanup_old_articles(conn: &Connection, days: i64) -> AppResult<usize> {
    // A retention window must be a positive number of days. A non-positive
    // value is meaningless and dangerous: `days = 0` builds the modifier
    // `'-0 days'`, so the cutoff `datetime('now', '-0 days')` collapses to
    // *now* and the DELETE purges **every** read article regardless of age;
    // a negative `days` builds a malformed `'--N days'` modifier that
    // `datetime()` evaluates to NULL, silently deleting nothing. Neither is a
    // real retention policy. The Settings UI only ever offers 30/90/180, but
    // this is the one chokepoint both that command and the background
    // scheduler funnel through — and the scheduler parses `days` from a
    // free-form settings string — so reject a non-positive value here rather
    // than trust every caller. Bail out as a no-op (0 removed).
    if days <= 0 {
        return Ok(0);
    }
    // Retention deletes only articles the user has not signalled they want to
    // keep. Starred and read-later are explicit "keep" flags; an article the
    // user has *highlighted* carries the same intent — the highlights table
    // cascade-deletes with the article (`ON DELETE CASCADE`), so purging a
    // highlighted-but-read article would silently destroy that hand-made
    // annotation layer (feature F7). Exempt any article with highlights.
    //
    // The purge condition — shared verbatim by the tombstone INSERT and the
    // DELETE so the two select exactly the same rows.
    const PURGE_WHERE: &str = "is_starred = 0 AND read_later = 0 AND is_read = 1
           AND NOT EXISTS (SELECT 1 FROM highlights WHERE article_id = articles.id)
           AND datetime(COALESCE(published_at, fetched_at)) < datetime('now', ?1)";
    let modifier = format!("-{days} days");

    // Tombstone every row about to be purged *before* deleting it, in one
    // transaction, so a full-archive feed can't re-insert it as fresh unread on
    // the next refresh (issue #98). `INSERT OR IGNORE` because a feed that
    // re-purges the same guid across cleanup runs already has its tombstone.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        &format!(
            "INSERT OR IGNORE INTO article_tombstones(feed_id, guid)
             SELECT feed_id, guid FROM articles WHERE {PURGE_WHERE}"
        ),
        params![modifier],
    )?;
    let removed = tx.execute(
        &format!("DELETE FROM articles WHERE {PURGE_WHERE}"),
        params![modifier],
    )?;
    tx.commit()?;
    Ok(removed)
}

/// Reclaim free pages — must run outside any transaction.
pub fn vacuum(conn: &Connection) -> AppResult<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// Wipe all user content (feeds → articles cascade, folders, tags, rules,
/// feed sources). Settings are kept. Deletes commit together so a failure
/// can't leave feeds wiped but folders / tags behind.
///
/// Also rebuilds the FTS5 index so shadow-table tombstones/segments collapse.
/// Do not `DELETE FROM articles_fts_*` directly — those tables are FTS5-
/// managed and hand-deleting them can corrupt the index.
pub fn clear_all_data(conn: &Connection) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    // Feeds first: articles (and join rows) cascade off feeds.
    tx.execute("DELETE FROM feeds", [])?;
    tx.execute("DELETE FROM folders", [])?;
    // Tags are independent of feeds — article_tags cascade when articles go,
    // but the tag rows themselves would otherwise linger empty.
    tx.execute("DELETE FROM tags", [])?;
    // Feed-scoped rules cascade with feeds; global rules (feed_id IS NULL)
    // would otherwise survive.
    tx.execute("DELETE FROM rules", [])?;
    
    tx.execute("DELETE FROM feed_sources", [])?;

    // Do not wipe `feed_sources` — those are curated directory-index configs
    // (Settings → 索引源), not subscription content.

    // FTS5 keeps deleted-doc tombstones in its shadow tables (`_data`, `_idx`)
    // after a mass DELETE. Rebuild collapses them so a cleared DB does not
    // keep hundreds of MB of dead index segments. `articles_fts_config` keeps
    // its single metadata row — that is required and expected.
    tx.execute("INSERT INTO articles_fts(articles_fts) VALUES('rebuild')", [])?;
    tx.commit()?;
    Ok(())
}

/// Clear every stored setting.
pub fn reset_settings(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM settings", [])?;
    Ok(())
}

pub fn count_unread(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM articles WHERE is_read = 0", [], |r| r.get(0))?)
}

/// Unread article count for a single feed — the same expression `list_feeds`
/// computes per row, used by `add_feed` so its returned `unread_count` matches
/// what the sidebar will show (rules that pre-mark an article read must not be
/// counted as unread).
pub fn count_feed_unread(conn: &Connection, feed_id: i64) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM articles WHERE feed_id = ?1 AND is_read = 0",
        params![feed_id],
        |r| r.get(0),
    )?)
}

/// Timestamp of the most recent successful feed fetch, if any.
pub fn latest_fetch(conn: &Connection) -> AppResult<Option<String>> {
    Ok(conn.query_row("SELECT MAX(last_fetched_at) FROM feeds", [], |r| {
        r.get::<_, Option<String>>(0)
    })?)
}

// ─────────────────────────── sync ───────────────────────────

/// Local article id for a given source URL — used to reconcile remote state.
pub fn article_id_by_url(conn: &Connection, url: &str) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM articles WHERE url = ?1 LIMIT 1",
            params![url],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_remote_id(conn: &Connection, article_id: i64, remote_id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET remote_id = ?2 WHERE id = ?1",
        params![article_id, remote_id],
    )?;
    Ok(())
}

/// Apply remote read/starred state to a local article.
pub fn set_sync_state(
    conn: &Connection,
    article_id: i64,
    read: bool,
    starred: bool,
) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET is_read = ?2, is_starred = ?3 WHERE id = ?1",
        params![article_id, read, starred],
    )?;
    Ok(())
}

/// Resolve a set of feed URLs to the ids of the local feeds that carry them,
/// skipping any URL the local DB doesn't track. Used to scope a sync
/// reconciliation to the feeds the server actually knows about.
pub fn feed_ids_by_urls(
    conn: &Connection,
    urls: &std::collections::HashSet<String>,
) -> AppResult<Vec<i64>> {
    let mut ids = Vec::with_capacity(urls.len());
    for url in urls {
        if let Some(id) = find_feed_by_url(conn, url)? {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Reconcile local read/starred state against the server's authoritative unread
/// + starred URL sets, for every article under one of `feed_ids`. An article is
/// marked read unless its URL is in `unread_urls`, and starred iff its URL is in
/// `starred_urls` — so the server's read state wins even for the long tail of
/// items a pull of only the recent unread set never enumerates (issue #96).
///
/// Articles carrying an un-pushed local edit (in `sync_queue`) are skipped, so a
/// pull never clobbers a change still waiting to be sent. Only rows whose state
/// actually changes are written. Returns the number of articles changed.
pub fn reconcile_sync_state(
    conn: &Connection,
    feed_ids: &[i64],
    unread_urls: &std::collections::HashSet<String>,
    starred_urls: &std::collections::HashSet<String>,
) -> AppResult<usize> {
    let pending: std::collections::HashSet<i64> =
        pending_sync_article_ids(conn)?.into_iter().collect();
    let tx = conn.unchecked_transaction()?;
    let mut changed: Vec<(i64, bool, bool)> = Vec::new();
    {
        let mut sel = tx.prepare(
            "SELECT id, url, is_read, is_starred FROM articles
             WHERE feed_id = ?1 AND url IS NOT NULL",
        )?;
        for &fid in feed_ids {
            let rows = sel.query_map(params![fid], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })?;
            for row in rows {
                let (id, url, cur_read, cur_starred) = row?;
                if pending.contains(&id) {
                    continue;
                }
                let read = !unread_urls.contains(&url);
                let starred = starred_urls.contains(&url);
                if read != cur_read || starred != cur_starred {
                    changed.push((id, read, starred));
                }
            }
        }
    }
    for (id, read, starred) in &changed {
        tx.execute(
            "UPDATE articles SET is_read = ?2, is_starred = ?3 WHERE id = ?1",
            params![id, read, starred],
        )?;
    }
    tx.commit()?;
    Ok(changed.len())
}

/// Queue a local read/starred change to push on the next sync.
pub fn enqueue_sync(
    conn: &Connection,
    article_id: i64,
    field: &str,
    value: bool,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO sync_queue(article_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value",
        params![article_id, field, value],
    )?;
    Ok(())
}

/// Article ids that still carry un-pushed local changes — their state must not
/// be overwritten by a pull until the change has been sent.
pub fn pending_sync_article_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT DISTINCT article_id FROM sync_queue")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// One pushable change drained from the sync queue.
pub struct SyncEntry {
    pub article_id: i64,
    pub remote_id: String,
    pub field: String,
    pub value: bool,
}

/// Drain pushable queue entries. Only rows whose article already has a remote
/// id are returned and removed; the rest wait for a pull to assign one. The
/// caller MUST re-queue any entry whose push fails (see `requeue_sync`) so a
/// network blip never silently drops a local change.
pub fn take_sync_queue(conn: &Connection) -> AppResult<Vec<SyncEntry>> {
    let mut stmt = conn.prepare(
        "SELECT q.article_id, a.remote_id, q.field, q.value
         FROM sync_queue q JOIN articles a ON a.id = q.article_id
         WHERE a.remote_id IS NOT NULL",
    )?;
    let rows: Vec<SyncEntry> = stmt
        .query_map([], |r| {
            Ok(SyncEntry {
                article_id: r.get(0)?,
                remote_id: r.get(1)?,
                field: r.get(2)?,
                value: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    conn.execute(
        "DELETE FROM sync_queue WHERE article_id IN
            (SELECT id FROM articles WHERE remote_id IS NOT NULL)",
        [],
    )?;
    Ok(rows)
}

/// Re-insert a queue entry whose push failed. Unlike `enqueue_sync` this does
/// not clobber a newer edit the user made on the same article during the sync.
pub fn requeue_sync(conn: &Connection, article_id: i64, field: &str, value: bool) -> AppResult<()> {
    conn.execute(
        "INSERT INTO sync_queue(article_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(article_id, field) DO NOTHING",
        params![article_id, field, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory database with all migrations applied and one feed +
    /// article inserted, so highlight FKs resolve. Returns `(conn, article_id)`.
    fn test_db() -> (Connection, i64) {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        MIGRATIONS.to_latest(&mut conn).unwrap();
        // The production `open` / `open_reader` register custom SQL functions;
        // the in-memory test connection must too so `preview_rule`'s
        // `unicode_lower` resolves.
        register_functions(&conn).unwrap();
        let feed_id = insert_feed(
            &conn,
            "https://example.com/feed.xml",
            None,
            "Example Feed",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let article = NewArticle {
            guid: "g1".into(),
            // URL follows the guid so helper seeds that use
            // `https://example.com/{guid}` (a1, a2, …) never collide with the
            // fixture under the partial unique URL index.
            url: Some("https://example.com/g1".into()),
            title: "An Article".into(),
            author: None,
            summary: None,
            content_html: Some("<p>body</p>".into()),
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &article, false, &[]).unwrap();
        let article_id: i64 = conn
            .query_row("SELECT id FROM articles", [], |r| r.get(0))
            .unwrap();
        (conn, article_id)
    }

    #[test]
    fn due_logic_respects_per_feed_and_global_intervals() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let global = 30;

        // Never fetched → always due, whatever the interval.
        assert!(feeds_due_for_refresh(&conn, global)
            .unwrap()
            .iter()
            .any(|(id, ..)| *id == feed_id));

        // Just fetched → not due under the 30-minute global interval.
        set_feed_fetch_state(&conn, feed_id, None, None, None).unwrap();
        assert!(
            feeds_due_for_refresh(&conn, global).unwrap().is_empty(),
            "a just-fetched feed should not be due"
        );

        // Backdate the fetch 20 minutes: still under the 30-minute global…
        conn.execute(
            "UPDATE feeds SET last_fetched_at = datetime('now', '-20 minutes') WHERE id = ?1",
            params![feed_id],
        )
        .unwrap();
        assert!(feeds_due_for_refresh(&conn, global).unwrap().is_empty());

        // …but a 15-minute per-feed override makes it due.
        set_feed_refresh_interval(&conn, feed_id, Some(15)).unwrap();
        assert!(!feeds_due_for_refresh(&conn, global).unwrap().is_empty());

        // The "off" sentinel opts the feed out entirely, even when overdue.
        set_feed_refresh_interval(&conn, feed_id, Some(REFRESH_OFF_MINUTES)).unwrap();
        assert!(feeds_due_for_refresh(&conn, global).unwrap().is_empty());

        // Clearing the override returns the feed to the global interval: with
        // the 20-minute-old fetch and a 5-minute global, it is due again.
        set_feed_refresh_interval(&conn, feed_id, None).unwrap();
        assert!(!feeds_due_for_refresh(&conn, 5).unwrap().is_empty());
    }

    #[test]
    fn open_mode_round_trips_and_clears() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        // Fresh feed follows the default.
        assert_eq!(list_feeds(&conn).unwrap()[0].open_mode, None);

        set_feed_open_mode(&conn, feed_id, Some("web")).unwrap();
        assert_eq!(
            list_feeds(&conn).unwrap()[0].open_mode.as_deref(),
            Some("web")
        );

        // `None` reverts to the default.
        set_feed_open_mode(&conn, feed_id, None).unwrap();
        assert_eq!(list_feeds(&conn).unwrap()[0].open_mode, None);
    }

    #[test]
    fn preview_translation_cache_is_keyed_by_language_and_engine() {
        let (conn, id) = test_db();

        set_preview_translation(&conn, id, "标题", "摘要", "zh", "google").unwrap();
        set_preview_translation(&conn, id, "題名", "要約", "ja", "google").unwrap();
        set_preview_translation(&conn, id, "見出し", "抜粋", "ja", "deepl").unwrap();

        assert_eq!(
            get_preview_translation(&conn, id, "zh", "google").unwrap(),
            Some(("标题".into(), "摘要".into()))
        );
        assert_eq!(
            get_preview_translation(&conn, id, "ja", "google").unwrap(),
            Some(("題名".into(), "要約".into()))
        );
        assert_eq!(
            get_preview_translation(&conn, id, "ja", "deepl").unwrap(),
            Some(("見出し".into(), "抜粋".into()))
        );
        assert_eq!(get_preview_translation(&conn, id, "en", "google").unwrap(), None);
    }

    #[test]
    fn preview_translation_cache_is_cleared_when_source_preview_changes() {
        let (conn, id) = test_db();

        set_preview_translation(&conn, id, "标题", "摘要", "zh", "google").unwrap();
        conn.execute(
            "UPDATE articles SET title = ?2, body_text = ?3 WHERE id = ?1",
            params![id, "New title", "New body"],
        )
        .unwrap();

        assert_eq!(get_preview_translation(&conn, id, "zh", "google").unwrap(), None);
    }

    #[test]
    fn translation_round_trips_through_get_article() {
        let (conn, id) = test_db();
        // No translation cached on a fresh article.
        let before = get_article(&conn, id).unwrap();
        assert_eq!(before.translated_html, None);
        assert_eq!(before.translated_lang, None);

        set_translation(&conn, id, "<p>译文</p>", "zh").unwrap();

        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.translated_html.as_deref(), Some("<p>译文</p>"));
        assert_eq!(after.translated_lang.as_deref(), Some("zh"));
    }

    #[test]
    fn set_translation_overwrites_a_previous_language() {
        let (conn, id) = test_db();
        set_translation(&conn, id, "<p>译文</p>", "zh").unwrap();
        set_translation(&conn, id, "<p>translation</p>", "en").unwrap();
        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.translated_html.as_deref(), Some("<p>translation</p>"));
        assert_eq!(after.translated_lang.as_deref(), Some("en"));
    }

    #[test]
    fn set_extracted_html_backfills_missing_image_url() {
        let (conn, id) = test_db();
        set_extracted_html(&conn, id, "<p>full text</p>", Some("https://ex.com/lead.jpg"))
            .unwrap();

        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.extracted_html.as_deref(), Some("<p>full text</p>"));
        assert_eq!(after.image_url.as_deref(), Some("https://ex.com/lead.jpg"));
    }

    #[test]
    fn set_extracted_html_keeps_existing_image_url() {
        let (conn, id) = test_db();
        conn.execute(
            "UPDATE articles SET image_url = ?2 WHERE id = ?1",
            params![id, "https://ex.com/feed.jpg"],
        )
        .unwrap();

        set_extracted_html(&conn, id, "<p>full text</p>", Some("https://ex.com/lead.jpg"))
            .unwrap();

        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.image_url.as_deref(), Some("https://ex.com/feed.jpg"));
    }

    #[test]
    fn card_image_backfill_scan_reads_extracted_html() {
        let (conn, id) = test_db();
        set_extracted_html(
            &conn,
            id,
            r#"<p>full text</p><img src="https://ex.com/from-extracted.jpg">"#,
            None,
        )
        .unwrap();

        let updates = card_image_backfill_scan(&conn).unwrap();
        assert_eq!(updates, vec![(id, "https://ex.com/from-extracted.jpg".into())]);
    }

    #[test]
    fn card_image_backfill_scan_includes_blank_image_url() {
        // A row whose `image_url` is blank (`''`, not NULL) must still be
        // scanned: `set_extracted_html`/`apply_card_images` treat NULL *or*
        // blank as missing, so a scan that only matched NULL would leave blank
        // rows permanently un-backfilled.
        let (conn, id) = test_db();
        conn.execute(
            "UPDATE articles
                SET image_url = '',
                    content_html = '<p>x</p><img src=\"https://ex.com/blank.jpg\">'
              WHERE id = ?1",
            [id],
        )
        .unwrap();

        let updates = card_image_backfill_scan(&conn).unwrap();
        assert_eq!(updates, vec![(id, "https://ex.com/blank.jpg".into())]);
    }

    /// Compact `NewHighlight` builder for the highlight tests.
    fn hl<'a>(
        article_id: i64,
        quote: &'a str,
        prefix: &'a str,
        suffix: &'a str,
        text_offset: i64,
        color: &'a str,
        note: &'a str,
    ) -> NewHighlight<'a> {
        NewHighlight {
            article_id,
            quote,
            prefix,
            suffix,
            text_offset,
            color,
            note,
        }
    }

    #[test]
    fn fresh_feed_has_no_last_fetched_until_touched() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        // A just-inserted feed has never been fetched.
        assert_eq!(feed_last_fetched(&conn, feed_id).unwrap(), None);
        // `touch_feed` (the same call `add_feed` makes after its initial
        // fetch) records the fetch time, so the feed no longer reads as
        // "never refreshed".
        touch_feed(&conn, feed_id).unwrap();
        assert!(feed_last_fetched(&conn, feed_id).unwrap().is_some());
    }

    #[test]
    fn refine_source_type_promotes_rss_but_never_demotes() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let kind = |c: &Connection| -> String {
            c.query_row("SELECT source_type FROM feeds WHERE id = ?1", params![feed_id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        // The test feed starts generic.
        assert_eq!(kind(&conn), "rss");

        // A no-op when the refined kind is still `Rss`.
        refine_feed_source_type(&conn, feed_id, SourceType::Rss).unwrap();
        assert_eq!(kind(&conn), "rss");

        // A genuine kind promotes the still-generic feed.
        refine_feed_source_type(&conn, feed_id, SourceType::Podcast).unwrap();
        assert_eq!(kind(&conn), "podcast");

        // Once classified, a later call must not churn the type — the
        // `WHERE source_type = 'rss'` guard makes this strictly a promotion.
        refine_feed_source_type(&conn, feed_id, SourceType::Mastodon).unwrap();
        assert_eq!(kind(&conn), "podcast");
    }

    #[test]
    fn opml_export_omits_newsletter_sources() {
        use crate::ingestion::newsletter::NewsletterConfig;
        let (conn, _) = test_db();
        // The RSS feed from `test_db` plus a newsletter source whose feed_url
        // is the synthetic, non-HTTP-fetchable `imap://` form.
        let cfg = NewsletterConfig {
            host: "imap.example.com".into(),
            port: 993,
            username: "me@example.com".into(),
            password: "secret".into(),
            folder: "Newsletters".into(),
        };
        insert_newsletter_source(
            &conn,
            "imap://me@example.com@imap.example.com:993/Newsletters",
            "My Newsletter",
            &cfg,
        )
        .unwrap();

        let exported = feeds_for_export(&conn).unwrap();
        // Only the real RSS feed is exportable — the newsletter is left out so
        // a re-import never resurrects it as a broken `imap://` RSS feed.
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].1, "https://example.com/feed.xml");
        assert!(
            !exported.iter().any(|(_, url, _)| url.starts_with("imap://")),
            "no synthetic imap:// url should reach the OPML"
        );
    }

    #[test]
    fn insert_and_list_highlight() {
        let (conn, aid) = test_db();
        let id = insert_highlight(&conn, &hl(aid, "quoted text", "pre", "suf", 12, "yellow", ""))
            .unwrap();
        let all = list_highlights(&conn, aid).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].quote, "quoted text");
        assert_eq!(all[0].prefix, "pre");
        assert_eq!(all[0].suffix, "suf");
        assert_eq!(all[0].text_offset, 12);
        assert_eq!(all[0].color, "yellow");
        assert_eq!(all[0].note, "");
    }

    #[test]
    fn highlights_ordered_by_offset() {
        let (conn, aid) = test_db();
        insert_highlight(&conn, &hl(aid, "third", "", "", 90, "yellow", "")).unwrap();
        insert_highlight(&conn, &hl(aid, "first", "", "", 10, "yellow", "")).unwrap();
        insert_highlight(&conn, &hl(aid, "second", "", "", 50, "yellow", "")).unwrap();
        let quotes: Vec<String> = list_highlights(&conn, aid)
            .unwrap()
            .into_iter()
            .map(|h| h.quote)
            .collect();
        assert_eq!(quotes, ["first", "second", "third"]);
    }

    #[test]
    fn update_note_and_color() {
        let (conn, aid) = test_db();
        let id = insert_highlight(&conn, &hl(aid, "q", "", "", 0, "yellow", "")).unwrap();
        update_highlight_note(&conn, id, "a thought").unwrap();
        set_highlight_color(&conn, id, "green").unwrap();
        let h = get_highlight(&conn, id).unwrap().unwrap();
        assert_eq!(h.note, "a thought");
        assert_eq!(h.color, "green");
    }

    #[test]
    fn delete_highlight_removes_it() {
        let (conn, aid) = test_db();
        let id = insert_highlight(&conn, &hl(aid, "q", "", "", 0, "yellow", "")).unwrap();
        delete_highlight(&conn, id).unwrap();
        assert!(list_highlights(&conn, aid).unwrap().is_empty());
        assert!(get_highlight(&conn, id).unwrap().is_none());
    }

    #[test]
    fn highlights_cascade_on_article_delete() {
        let (conn, aid) = test_db();
        insert_highlight(&conn, &hl(aid, "q", "", "", 0, "yellow", "")).unwrap();
        conn.execute("DELETE FROM articles WHERE id = ?1", params![aid])
            .unwrap();
        assert!(list_highlights(&conn, aid).unwrap().is_empty());
    }

    #[test]
    fn list_all_highlights_spans_articles() {
        let (conn, aid) = test_db();
        insert_highlight(&conn, &hl(aid, "one", "", "", 0, "yellow", "")).unwrap();
        insert_highlight(&conn, &hl(aid, "two", "", "", 5, "green", "noted")).unwrap();
        assert_eq!(list_all_highlights(&conn).unwrap().len(), 2);
    }

    // ── FTS query building ───────────────────────────────────────────

    #[test]
    fn fts_query_and_joins_explicit_search_terms() {
        // Explicit search: every word required (implicit FTS5 AND).
        assert_eq!(fts_query("rust async", false), "\"rust\"* AND \"async\"*");
    }

    #[test]
    fn fts_query_or_joins_for_recall() {
        // RAG retrieval: any word may match.
        assert_eq!(
            fts_query("rust async runtime", true),
            "(\"rust\"* OR \"async\"* OR \"runtime\"*)"
        );
    }

    #[test]
    fn fts_query_strips_punctuation_and_handles_empty() {
        // Non-alphanumerics are dropped from each term; an all-punctuation
        // input collapses to a match-nothing expression in both modes.
        // Short Latin tokens (≤3 chars) are whole-token matches — no `*`
        // prefix — so `c++` does not over-match `c`+anything.
        assert_eq!(fts_query("c++!", false), "\"c\"");
        assert_eq!(fts_query("!!! ???", true), "\"\"");
        assert_eq!(fts_query("   ", false), "\"\"");
    }

    #[test]
    fn fts_query_splits_punctuation_into_separate_terms() {
        // Punctuation *inside* a word splits it into separate AND-joined
        // parts, matching how unicode61 indexes the article text.
        assert_eq!(fts_query("rust-lang", false), "\"rust\"* AND \"lang\"*");
        // A single dotted token still AND-joins its parts (not OR), even in
        // recall mode — recall only ORs *separate* bare terms. Short parts
        // (`js`, `co`, `op`) stay whole-token (no `*`); longer parts keep the
        // automatic prefix.
        assert_eq!(fts_query("node.js", true), "\"node\"* AND \"js\"");
        assert_eq!(
            fts_query("co-op runtime", false),
            "\"co\" AND \"op\" AND \"runtime\"*"
        );
    }

    /// Insert a second article with searchable text for the RAG tests.
    fn add_article(conn: &Connection, feed_id: i64, guid: &str, title: &str, body: &str) {
        let article = NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: Some(format!("<p>{body}</p>")),
            body_text: body.into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(conn, feed_id, &article, false, &[]).unwrap();
    }

    #[test]
    fn rag_search_matches_any_keyword_not_all() {
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        add_article(&conn, feed_id, "rust", "Rust news", "the borrow checker explained");
        add_article(&conn, feed_id, "privacy", "Privacy law", "a new data privacy regulation");

        // A natural-language question shares only *some* words with each
        // article. An AND join would require every word to appear and return
        // nothing; the OR-based RAG search still finds both relevant pieces.
        let hits = search_articles_for_rag(
            &conn,
            "what does the new privacy regulation say about the borrow checker",
            6,
        )
        .unwrap();
        let titles: Vec<&str> = hits.iter().map(|(_, t, _)| t.as_str()).collect();
        assert!(titles.contains(&"Rust news"), "got: {titles:?}");
        assert!(titles.contains(&"Privacy law"), "got: {titles:?}");
    }

    #[test]
    fn rag_search_empty_question_returns_no_rows() {
        let (conn, _aid) = test_db();
        // An all-stopword / punctuation-only question must not error and must
        // return nothing (the match-nothing `""` expression).
        assert!(search_articles_for_rag(&conn, "??? !!!", 6).unwrap().is_empty());
    }

    #[test]
    fn create_tag_is_idempotent_on_name() {
        let (conn, _aid) = test_db();
        let first = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        // Re-creating the same name returns the existing id, not a constraint
        // error, and does not add a second row.
        let again = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        assert_eq!(first, again);
        // Case-insensitive: "rust" resolves to the same tag as "Rust".
        let cased = create_tag(&conn, "rust", TAG_KIND_INTEREST).unwrap();
        assert_eq!(first, cased);
        assert_eq!(list_tags(&conn, None).unwrap().len(), 1);
    }

    #[test]
    fn create_tag_kinds_are_separate_taxonomies() {
        let (conn, _aid) = test_db();
        let interest = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        let ai = create_tag(&conn, "Rust", TAG_KIND_AI).unwrap();
        assert_ne!(interest, ai);
        assert_eq!(
            list_tags(&conn, Some(TAG_KIND_INTEREST)).unwrap().len(),
            1
        );
        assert_eq!(list_tags(&conn, Some(TAG_KIND_AI)).unwrap().len(), 1);
        // Rename within AI must not clash with the interest twin.
        rename_tag(&conn, ai, "RustLang").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM tags WHERE id = ?1", [ai], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "RustLang");
    }

    #[test]
    fn create_tag_trims_whitespace_and_dedups_padded_names() {
        // A tag name with surrounding whitespace must resolve to the same tag
        // as its trimmed form — otherwise the `COLLATE NOCASE` lookup misses
        // and a visually identical near-duplicate tag is created.
        let (conn, _aid) = test_db();
        let rust = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        assert_eq!(create_tag(&conn, "  Rust  ", TAG_KIND_INTEREST).unwrap(), rust);
        assert_eq!(create_tag(&conn, "\tRust\n", TAG_KIND_INTEREST).unwrap(), rust);
        // The stored name is the trimmed form.
        let go = create_tag(&conn, "  Go ", TAG_KIND_INTEREST).unwrap();
        let name: String = conn
            .query_row("SELECT name FROM tags WHERE id = ?1", [go], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Go");
        assert_eq!(list_tags(&conn, None).unwrap().len(), 2);
    }

    #[test]
    fn rename_tag_rejects_whitespace_padded_collision() {
        // A rename to a whitespace-padded variant of another tag's name must
        // still be rejected — the trim lets the clash check see through it.
        let (conn, _aid) = test_db();
        let _rust = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        let go = create_tag(&conn, "Go", TAG_KIND_INTEREST).unwrap();
        let err = rename_tag(&conn, go, "  Rust  ").unwrap_err();
        assert!(matches!(err, AppError::Coded("tagNameExists")));
    }

    #[test]
    fn create_tag_after_middle_delete_sorts_last() {
        // Deleting a tag from the middle of the list leaves a gap in the
        // `position` sequence. A new tag must still land at the end — a
        // `COUNT(*)`-based position would collide with an existing row.
        let (conn, _aid) = test_db();
        let a = create_tag(&conn, "alpha", TAG_KIND_INTEREST).unwrap();
        let b = create_tag(&conn, "beta", TAG_KIND_INTEREST).unwrap();
        let _c = create_tag(&conn, "gamma", TAG_KIND_INTEREST).unwrap();
        delete_tag(&conn, b).unwrap();
        let zoo = create_tag(&conn, "zeta", TAG_KIND_INTEREST).unwrap();

        let order: Vec<i64> = list_tags(&conn, None).unwrap().iter().map(|t| t.id).collect();
        assert_eq!(
            order.last(),
            Some(&zoo),
            "a tag created after a middle delete must sort last, got {order:?}",
        );
        // The pre-existing tags keep their relative order.
        assert!(
            order.iter().position(|&x| x == a) < order.iter().position(|&x| x == zoo),
        );
    }

    #[test]
    fn merge_tags_moves_articles_and_dedups() {
        let (conn, feed_id) = test_db();
        // Two extra articles so the source tag has multiple attachments.
        for i in 0..2 {
            let a = NewArticle {
                guid: format!("m{i}"),
                url: Some(format!("https://example.com/m{i}")),
                title: "Merge test".into(),
                author: None,
                summary: None,
                content_html: None,
                body_text: "b".into(),
                image_url: None,
                published_at: None,
                enclosures: Vec::new(),
            };
            upsert_article(&conn, feed_id, &a, false, &[]).unwrap();
        }
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM articles ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let source = create_tag(&conn, "中东", TAG_KIND_AI).unwrap();
        let target = create_tag(&conn, "Middle East", TAG_KIND_AI).unwrap();
        let interest = create_tag(&conn, "中东局势", TAG_KIND_INTEREST).unwrap();
        // Source: articles 1,2,3. Target already has article 2 (dedup case).
        for id in &ids {
            set_article_tag(&conn, *id, source, true).unwrap();
        }
        set_article_tag(&conn, ids[1], target, true).unwrap();

        // Same-kind merge moves the two new attachments and keeps the dup.
        let moved = merge_tags(&conn, source, target).unwrap();
        assert_eq!(moved, 2, "article 2 already carried the target tag");
        let target_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM article_tags WHERE tag_id = ?1",
                params![target],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target_count, ids.len() as i64);
        // Source tag is gone.
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM tags WHERE id = ?1", params![source], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);

        // Cross-kind merges are rejected.
        let err = merge_tags(&conn, target, interest).unwrap_err();
        assert!(matches!(err, AppError::Coded("tagKindMismatch")));
        // Unknown ids are rejected too.
        assert!(merge_tags(&conn, 99999, target).is_err());
    }

    #[test]
    fn delete_empty_tags_removes_only_unused_ai() {
        let (conn, article_id) = test_db();
        let empty_ai = create_tag(&conn, "orphan-ai", TAG_KIND_AI).unwrap();
        let used_ai = create_tag(&conn, "used-ai", TAG_KIND_AI).unwrap();
        let empty_interest = create_tag(&conn, "orphan-interest", TAG_KIND_INTEREST).unwrap();
        set_article_tag(&conn, article_id, used_ai, true).unwrap();

        let deleted = delete_empty_tags(&conn, TAG_KIND_AI).unwrap();
        assert_eq!(deleted, 1);
        assert!(list_tags(&conn, Some(TAG_KIND_AI))
            .unwrap()
            .iter()
            .all(|t| t.id != empty_ai));
        assert!(list_tags(&conn, Some(TAG_KIND_AI))
            .unwrap()
            .iter()
            .any(|t| t.id == used_ai));
        // Empty interest vocabulary is never cleaned up.
        assert!(list_tags(&conn, Some(TAG_KIND_INTEREST))
            .unwrap()
            .iter()
            .any(|t| t.id == empty_interest));
    }

    #[test]
    fn delete_empty_tags_rejects_interest_kind() {
        let (conn, _aid) = test_db();
        let _ = create_tag(&conn, "keep-me", TAG_KIND_INTEREST).unwrap();
        let err = delete_empty_tags(&conn, TAG_KIND_INTEREST).unwrap_err();
        assert!(matches!(
            err,
            AppError::Coded("cleanupEmptyInterestForbidden")
        ));
        assert_eq!(
            list_tags(&conn, Some(TAG_KIND_INTEREST)).unwrap().len(),
            1
        );
    }

    #[test]
    fn create_rule_after_middle_delete_sorts_last() {
        // Same `COUNT(*)`-vs-`MAX(position)` hazard as tags: deleting a rule
        // from the middle leaves a `position` gap, so a `COUNT(*)`-based
        // position collides with an existing row and the new rule no longer
        // sorts last. Rule order is load-bearing for ingestion evaluation.
        let (conn, _aid) = test_db();
        // Five rules, positions {0,1,2,3,4}.
        let _a = create_rule(&conn, "alpha", None, "title", "x", "skip").unwrap();
        let b = create_rule(&conn, "beta", None, "title", "y", "skip").unwrap();
        let c = create_rule(&conn, "gamma", None, "title", "z", "skip").unwrap();
        let _d = create_rule(&conn, "delta", None, "title", "w", "skip").unwrap();
        let _e = create_rule(&conn, "epsilon", None, "title", "u", "skip").unwrap();
        // Delete two from the middle, leaving positions {0,3,4} — wide enough
        // that a `COUNT(*)` value (3) collides with a non-last rule.
        delete_rule(&conn, b).unwrap();
        delete_rule(&conn, c).unwrap();
        let zoo = create_rule(&conn, "zeta", None, "title", "v", "skip").unwrap();

        let order: Vec<i64> = list_rules(&conn).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(
            order.last(),
            Some(&zoo),
            "a rule created after a middle delete must sort last, got {order:?}",
        );
    }

    // ── per-feed unread count ────────────────────────────────────────

    #[test]
    fn count_feed_unread_excludes_articles_pre_marked_read_by_a_rule() {
        // A filter rule with a `read` action inserts a matching article
        // already marked read. `count_feed_unread` must agree with
        // `list_feeds`: it counts only genuinely-unread rows.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        // The fixture article is unread.
        assert_eq!(count_feed_unread(&conn, feed_id).unwrap(), 1);

        // A rule that pre-marks anything titled "Sponsored" as read.
        create_rule(&conn, "ads", None, "title", "Sponsored", "read").unwrap();
        let rules = active_rules(&conn).unwrap();

        let read_by_rule = NewArticle {
            guid: "g-sponsored".into(),
            url: Some("https://example.com/sponsored".into()),
            title: "Sponsored Post".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "ad copy".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        let plain = NewArticle {
            guid: "g-plain".into(),
            url: Some("https://example.com/plain".into()),
            title: "A Normal Post".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "ordinary copy".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        // Both rows land, but `upsert_article` returns `true` only for the
        // genuinely-unread one — the rule-read article is not "new".
        assert!(
            !upsert_article(&conn, feed_id, &read_by_rule, false, &rules).unwrap(),
            "an article pre-marked read by a rule is not a new unread article"
        );
        assert!(upsert_article(&conn, feed_id, &plain, false, &rules).unwrap());

        // Three articles inserted total, but the rule-read one is not unread.
        assert_eq!(
            count_feed_unread(&conn, feed_id).unwrap(),
            2,
            "the rule-read article must not be counted as unread"
        );
        // And it matches the count `list_feeds` computes for the same feed.
        let from_list = list_feeds(&conn)
            .unwrap()
            .into_iter()
            .find(|f| f.id == feed_id)
            .unwrap()
            .unread_count;
        assert_eq!(from_list, 2);
    }

    #[test]
    fn ingest_indexing_survives_overlapping_cjk_aliases() {
        // Regression for the production outage: `index_article_by_id` (the
        // backfill indexing path, and the same `terms_for_snippet` chain the
        // ingest path runs) used to panic inside wordcloud `match_entities`
        // when a shorter CJK alias (美国) is rejected because a longer alias
        // (美国国防部) already occupied the span — the byte-offset advance
        // landed mid-UTF-8-char. Deterministic here because the dict is
        // constructed inline rather than loaded from the process default.
        use crate::wordcloud_dict::{EntitiesFile, WordCloudDict};
        use std::path::PathBuf;

        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![
                crate::wordcloud_dict::WordCloudEntity {
                    id: "org.pentagon".into(),
                    canonical: "Pentagon".into(),
                    group: "org".into(),
                    aliases: vec!["美国国防部".into()],
                },
                crate::wordcloud_dict::WordCloudEntity {
                    id: "country.china".into(),
                    canonical: "China".into(),
                    group: "country".into(),
                    aliases: vec!["美国".into()],
                },
            ],
        });

        let (conn, feed_id) = test_db();
        let cjk = NewArticle {
            guid: "cjk-overlap".into(),
            url: Some("https://example.com/cjk-overlap".into()),
            title: "美国国防部 invests heavily".into(),
            author: None,
            summary: None,
            content_html: Some("<p>body</p>".into()),
            body_text: "Pentagon spending news".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &cjk, false, &[]).unwrap();
        let article_id: i64 = conn
            .query_row(
                "SELECT id FROM articles WHERE guid = 'cjk-overlap'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Must not panic; must index the longer-alias entity.
        crate::wordcloud::index_article_by_id(&conn, article_id, &dict).unwrap();
        let terms: Vec<String> = conn
            .prepare("SELECT term FROM article_terms WHERE article_id = ?1")
            .unwrap()
            .query_map([article_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            terms.iter().any(|t| t == "Pentagon"),
            "expected the longer-alias canonical in terms, got {terms:?}"
        );
    }

    #[test]
    fn cap_newest_articles_keeps_recent_by_date_in_original_order() {
        let mk = |guid: &str, published: Option<&str>| NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "b".into(),
            image_url: None,
            published_at: published.map(|p| p.to_string()),
            enclosures: Vec::new(),
        };
        // Dates deliberately out of document order to prove the selection is
        // date-based (newest kept), not first-N; the missing-date item is oldest.
        let mut articles = vec![
            mk("jan", Some("2026-01-01T00:00:00+00:00")),
            mk("mar", Some("2026-03-01T00:00:00+00:00")),
            mk("nodate", None),
            mk("feb", Some("2026-02-01T00:00:00+00:00")),
        ];
        cap_newest_articles(&mut articles, 2);
        let kept: Vec<&str> = articles.iter().map(|a| a.guid.as_str()).collect();
        // Newest two by date: mar (03-01) and feb (02-01), in original order.
        assert_eq!(kept, vec!["mar", "feb"]);

        // cap == 0 is a no-op; oversized caps are a no-op too.
        let mut all = vec![mk("a", None), mk("b", None)];
        cap_newest_articles(&mut all, 0);
        assert_eq!(all.len(), 2);
        cap_newest_articles(&mut all, 99);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn stale_articles_are_not_enqueued_for_auto_tag() {
        // A newly-added feed backfills its history on first fetch; those old
        // items must not each trigger a full-price LLM tagging call.
        let (conn, feed_id) = test_db();
        set_setting(&conn, "ai_tag_enabled", "1").unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let fresh = chrono::Utc::now().to_rfc3339();
        let mk = |guid: &str, published: String| NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "b".into(),
            image_url: None,
            published_at: Some(published),
            enclosures: Vec::new(),
        };
        assert!(upsert_article(&conn, feed_id, &mk("old", old), false, &[]).unwrap());
        assert!(upsert_article(&conn, feed_id, &mk("fresh", fresh), false, &[]).unwrap());

        let queued: Vec<(i64, String)> = conn
            .prepare("SELECT q.article_id, a.guid FROM auto_tag_queue q JOIN articles a ON a.id = q.article_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(queued.len(), 1, "only the fresh article may be enqueued");
        assert_eq!(queued[0].1, "fresh");
    }

    #[test]
    fn count_ai_usage_today_counts_only_today() {
        let (conn, _) = test_db();
        conn.execute(
            "INSERT INTO ai_usage(feature, prompt_tokens) VALUES ('auto-tag', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_usage(feature, prompt_tokens, created_at) \
             VALUES ('auto-tag', 10, datetime('now', '-2 days'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_usage(feature, prompt_tokens) VALUES ('summarize', 10)",
            [],
        )
        .unwrap();
        assert_eq!(count_ai_usage_today(&conn, "auto-tag").unwrap(), 1);
    }

    #[test]
    fn digest_cache_respects_ttl() {
        let (conn, _) = test_db();
        assert!(get_digest_cache(&conn).unwrap().is_none(), "empty cache");

        set_digest_cache(&conn, "Fresh briefing").unwrap();
        assert_eq!(
            get_digest_cache(&conn).unwrap().as_deref(),
            Some("Fresh briefing")
        );

        // Expire the entry: a stale cache must be ignored (regenerate).
        conn.execute(
            "UPDATE settings SET value = datetime('now', '-2 hours') WHERE key = 'digest_cache_at'",
            [],
        )
        .unwrap();
        assert!(
            get_digest_cache(&conn).unwrap().is_none(),
            "stale digest must not be served"
        );

        // Empty text is never served as a hit.
        set_digest_cache(&conn, "   ").unwrap();
        assert!(get_digest_cache(&conn).unwrap().is_none());
    }

    #[test]
    fn balance_history_derives_spend_and_topups() {
        let (conn, _) = test_db();
        assert!(latest_balance(&conn).unwrap().is_none());
        assert!(last_balance_day(&conn).unwrap().is_none());
        assert!(balance_history(&conn, 30).unwrap().is_empty());

        // Three snapshots: day1 = 100, day2 = 80 (spent 20), day3 = 110 (topup 30).
        conn.execute(
            "INSERT INTO ai_balance_history(recorded_at, total_balance, granted_balance, topped_up_balance)
             VALUES ('2026-08-01', 100, 0, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_balance_history(recorded_at, total_balance, granted_balance, topped_up_balance)
             VALUES ('2026-08-02', 80, 0, 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_balance_history(recorded_at, total_balance, granted_balance, topped_up_balance)
             VALUES ('2026-08-03', 110, 0, 130)",
            [],
        )
        .unwrap();

        let latest = latest_balance(&conn).unwrap().unwrap();
        assert_eq!(latest.day, "2026-08-03");
        assert_eq!(latest.total_balance, 110.0);
        assert_eq!(last_balance_day(&conn).unwrap().as_deref(), Some("2026-08-03"));

        let hist = balance_history(&conn, 30).unwrap();
        assert_eq!(hist.len(), 3);
        // Oldest first; first day has no baseline.
        assert_eq!(hist[0].day, "2026-08-01");
        assert!(hist[0].spend.is_none());
        // day2: spend 20.
        assert_eq!(hist[1].day, "2026-08-02");
        assert_eq!(hist[1].spend, Some(20.0));
        assert!(hist[1].topup.is_none());
        // day3: topup 30, no spend.
        assert_eq!(hist[2].day, "2026-08-03");
        assert_eq!(hist[2].topup, Some(30.0));
        assert!(hist[2].spend.is_none());

        // Official usage round-trips.
        upsert_official_usage(&conn, "2026-08-02", 100_000, 1.5).unwrap();
        upsert_official_usage(&conn, "2026-08-02", 120_000, 1.8).unwrap(); // upsert
        let usage = official_usage(&conn, 30).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].day, "2026-08-02");
        assert_eq!(usage[0].tokens, 120_000);
        assert_eq!(usage[0].cost, 1.8);
    }

    #[test]
    fn upsert_article_reports_rule_read_inserts_as_not_new() {
        // The refresh scheduler tallies `upsert_article(..) == Ok(true)` into
        // the "N new articles" count that drives the refresh toast and the OS
        // notification. An article inserted but pre-marked read by a `read`
        // rule never appears as unread, so it must NOT be counted as new —
        // otherwise the toast/notification claims new articles the user can
        // never find.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        create_rule(&conn, "ads", None, "title", "Sponsored", "read").unwrap();
        let rules = active_rules(&conn).unwrap();

        let mk = |guid: &str, title: &str| NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "copy".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };

        // Pre-marked read by the rule → not new.
        assert!(!upsert_article(&conn, feed_id, &mk("g-ad", "Sponsored Item"), false, &rules)
            .unwrap());
        // A plain article → genuinely new.
        assert!(upsert_article(&conn, feed_id, &mk("g-ok", "Real Story"), false, &rules)
            .unwrap());
        // A duplicate guid → not new (no double count).
        assert!(!upsert_article(&conn, feed_id, &mk("g-ok", "Real Story"), false, &rules)
            .unwrap());
    }

    // ── rule matching ────────────────────────────────────────────────

    #[test]
    fn any_field_rule_does_not_match_keyword_across_field_boundary() {
        // An `any`-field rule must check each field independently — the same
        // per-column semantics `preview_rule` uses. A keyword that only exists
        // because the title's tail and the body's head happen to abut must NOT
        // fire the rule; otherwise live ingestion acts on articles the rule
        // preview never counted.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();

        // A `skip` rule keyed on the two-word phrase "rust weekly".
        create_rule(&conn, "rw", None, "any", "rust weekly", "skip").unwrap();
        let rules = active_rules(&conn).unwrap();

        // Title ends in "rust", author starts with "weekly": the old code
        // concatenated `title author body` with single spaces, so the phrase
        // appeared only at that join. Per-field matching must not fire here,
        // and the article must still insert.
        let straddle = NewArticle {
            guid: "g-straddle".into(),
            url: Some("https://example.com/straddle".into()),
            title: "All about rust".into(),
            author: Some("Weekly Digest".into()),
            summary: None,
            content_html: None,
            body_text: "body text".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(
            upsert_article(&conn, feed_id, &straddle, false, &rules).unwrap(),
            "a keyword straddling the title/author boundary must not skip the article"
        );

        // The phrase wholly within one field still triggers the skip.
        let within = NewArticle {
            guid: "g-within".into(),
            url: Some("https://example.com/within".into()),
            title: "The Rust Weekly roundup".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "intro text".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(
            !upsert_article(&conn, feed_id, &within, false, &rules).unwrap(),
            "a keyword wholly within one field must still skip the article"
        );
    }

    #[test]
    fn preview_rule_case_folds_non_ascii_like_live_ingestion() {
        // `rule_matches` folds case with Rust's Unicode-aware `to_lowercase()`,
        // so a `café` rule matches a `CAFÉ` article during ingestion. SQLite's
        // built-in `LOWER()` is ASCII-only and would leave `É` uppercase,
        // making the preview undercount — `preview_rule` must use the
        // Unicode-aware `unicode_lower` so its count agrees with ingestion.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();

        // An article whose title carries an upper-case non-ASCII letter.
        let article = NewArticle {
            guid: "g-cafe".into(),
            url: Some("https://example.com/cafe".into()),
            title: "CAFÉ CULTURE in Zürich".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(upsert_article(&conn, feed_id, &article, false, &[]).unwrap());

        // Lower-case non-ASCII keywords must still count the article.
        for keyword in ["café", "zürich"] {
            let (count, samples) =
                preview_rule(&conn, None, "title", keyword).unwrap();
            assert_eq!(count, 1, "keyword `{keyword}` should match the CAFÉ article");
            assert_eq!(samples.len(), 1);
        }

        // And the rule that the preview describes must agree at ingest time:
        // a `skip` rule on `café` drops a fresh `CAFÉ`-titled article.
        create_rule(&conn, "no-cafe", None, "title", "café", "skip").unwrap();
        let rules = active_rules(&conn).unwrap();
        let fresh = NewArticle {
            guid: "g-cafe-2".into(),
            url: Some("https://example.com/cafe2".into()),
            title: "Another CAFÉ Story".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(
            !upsert_article(&conn, feed_id, &fresh, false, &rules).unwrap(),
            "ingestion must skip the article the `café` preview counted"
        );
    }

    // ── apply_rule_to_existing (retroactive backfill on save) ─────────

    fn seed(conn: &Connection, feed_id: i64, guid: &str, title: &str) {
        let a = NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(conn, feed_id, &a, false, &[]).unwrap();
    }

    #[test]
    fn apply_rule_to_existing_stars_matching_backlog_idempotently() {
        // The bug this guards: saving a `star` rule did nothing to articles
        // already stored — only `upsert_article` ran the rule, and that fires
        // only on freshly fetched articles. `apply_rule_to_existing` backfills
        // the matches when the rule is saved.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        seed(&conn, feed_id, "j1", "Learning Java today");
        seed(&conn, feed_id, "j2", "JavaScript tips");
        seed(&conn, feed_id, "p1", "Rust ownership");

        let n = apply_rule_to_existing(&conn, None, "title", "java", "star").unwrap();
        assert_eq!(n, 2, "both Java/JavaScript titles get starred");
        let starred: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles WHERE is_starred = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(starred, 2);

        // Re-running counts only rows it actually changes — already-starred rows
        // are excluded, so a second save reports 0.
        let again = apply_rule_to_existing(&conn, None, "title", "java", "star").unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn apply_rule_to_existing_skip_deletes_matches_keeping_fts_in_sync() {
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        seed(&conn, feed_id, "a1", "Sponsored junk");
        seed(&conn, feed_id, "a2", "more Sponsored stuff");
        seed(&conn, feed_id, "a3", "real content");

        // The preview count is exactly the set the skip apply deletes — they
        // share `rule_match_where`, so the user is never surprised.
        let (preview, _) = preview_rule(&conn, None, "title", "sponsored").unwrap();
        assert_eq!(preview, 2);
        let n = apply_rule_to_existing(&conn, None, "title", "sponsored", "skip").unwrap();
        assert_eq!(n, 2);

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE title LIKE '%Sponsored%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "matching articles are deleted");

        // The `articles_fts_ad` trigger drops the FTS rows with the articles, so
        // the index never goes stale.
        let arts: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))
            .unwrap();
        let fts: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(arts, fts, "FTS index stays in sync after a skip-rule delete");
    }

    #[test]
    fn apply_rule_to_existing_scopes_to_one_feed() {
        let (conn, _aid) = test_db();
        let feed_a: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let feed_b = insert_feed(
            &conn,
            "https://example.com/b.xml",
            None,
            "Feed B",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        seed(&conn, feed_a, "x1", "Deals everywhere");
        seed(&conn, feed_b, "x2", "Deals galore");

        // Scoped to feed B only: feed A's match is left untouched.
        let n = apply_rule_to_existing(&conn, Some(feed_b), "title", "deals", "star").unwrap();
        assert_eq!(n, 1);
        let starred_in_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE is_starred = 1 AND feed_id = ?1",
                params![feed_a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(starred_in_a, 0, "a feed-scoped rule must not touch other feeds");
    }

    // ── mark_all_read sync queueing ──────────────────────────────────

    #[test]
    fn mark_all_read_queues_articles_without_a_remote_id() {
        // Freshly fetched articles carry no `remote_id` until a sync pull
        // matches them by URL. A bulk "mark all read" run right after a
        // refresh must still queue those changes so they reach the sync
        // server once the id is assigned — not silently drop them.
        let (conn, aid) = test_db();
        assert_eq!(
            conn.query_row("SELECT remote_id FROM articles WHERE id = ?1", [aid], |r| r
                .get::<_, Option<String>>(0))
                .unwrap(),
            None,
            "fixture article should start without a remote id"
        );

        let n = mark_all_read(&conn, &ArticleQuery::All, true).unwrap();
        assert_eq!(n, 1, "the one unread article should be flipped to read");

        let queued: i64 = conn
            .query_row(
                "SELECT count(*) FROM sync_queue WHERE article_id = ?1 AND field = 'read'",
                [aid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 1, "the read change must be queued for sync");
    }

    #[test]
    fn mark_all_read_skips_sync_queue_when_not_connected() {
        // Without a sync server linked, no rows should land in the queue.
        let (conn, _aid) = test_db();
        mark_all_read(&conn, &ArticleQuery::All, false).unwrap();
        let queued: i64 = conn
            .query_row("SELECT count(*) FROM sync_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 0);
    }

    // ── sync state reconciliation (issue #96) ────────────────────────

    #[test]
    fn reconcile_marks_tail_read_and_scopes_to_server_feeds() {
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        // Fixture article a1 (guid g1) plus two more in the same server feed —
        // all start unread, as a raw RSS poll would leave them.
        seed(&conn, feed_id, "a2", "Two");
        seed(&conn, feed_id, "a3", "Three");
        // A local-only feed the server doesn't know about.
        let local = insert_feed(
            &conn,
            "https://local.example/feed.xml",
            None,
            "Local",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        seed(&conn, local, "b1", "Local one");

        // Server: only a2 is unread, a3 is starred.
        let unread: std::collections::HashSet<String> =
            ["https://example.com/a2".to_string()].into_iter().collect();
        let starred: std::collections::HashSet<String> =
            ["https://example.com/a3".to_string()].into_iter().collect();

        let changed = reconcile_sync_state(&conn, &[feed_id], &unread, &starred).unwrap();
        assert_eq!(changed, 2, "a1 -> read and a3 -> read+starred; a2 unchanged");

        let read = |g: &str| -> bool {
            conn.query_row("SELECT is_read FROM articles WHERE guid = ?1", [g], |r| r.get(0))
                .unwrap()
        };
        let is_starred = |g: &str| -> bool {
            conn.query_row("SELECT is_starred FROM articles WHERE guid = ?1", [g], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert!(read("g1"), "tail item not in unread set is marked read");
        assert!(!read("a2"), "server-unread item stays unread");
        assert!(read("a3") && is_starred("a3"), "a3 is read and starred");
        assert!(!read("b1"), "local-only feed is out of scope and untouched");
    }

    #[test]
    fn reconcile_skips_pending_local_edits() {
        let (conn, aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        // User marked a1 unread locally; the change hasn't been pushed yet.
        enqueue_sync(&conn, aid, "read", false).unwrap();
        // Server considers a1 read (empty unread set) — but the pending local
        // edit must win, so nothing changes.
        let empty = std::collections::HashSet::new();
        let changed = reconcile_sync_state(&conn, &[feed_id], &empty, &empty).unwrap();
        assert_eq!(changed, 0, "pending article is skipped");
        let read: bool = conn
            .query_row("SELECT is_read FROM articles WHERE id = ?1", [aid], |r| r.get(0))
            .unwrap();
        assert!(!read, "a1 keeps its local (unread) state");
    }

    // ── retention cleanup ────────────────────────────────────────────

    /// Insert a read article with an explicit RFC 3339 `published_at`, the
    /// format `to_rfc3339` produces for every feed-dated article.
    fn insert_read_article_published(conn: &Connection, feed_id: i64, guid: &str, rfc3339: &str) {
        let a = NewArticle {
            guid: guid.into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: Some(rfc3339.into()),
            enclosures: Vec::new(),
        };
        upsert_article(conn, feed_id, &a, false, &[]).unwrap();
        conn.execute(
            "UPDATE articles SET is_read = 1 WHERE guid = ?1",
            params![guid],
        )
        .unwrap();
    }

    #[test]
    fn cleanup_compares_rfc3339_published_at_by_real_instant() {
        // `published_at` is stored RFC 3339 (`...T...+00:00`) while the
        // retention cutoff uses SQLite's space-separated form. A raw string
        // `<` mis-orders the two: the `T` byte sorts *after* a space, so for
        // an article whose `published_at` falls on the same calendar day as
        // the cutoff but earlier in the day, the string compare wrongly
        // reports it as newer and it escapes deletion. The fix normalises
        // both sides with `datetime()`.
        //
        // This test pins exactly that same-day boundary: an article dated to
        // the cutoff's own calendar day but earlier in that day — genuinely
        // outside a 30-day window, yet a string compare wrongly keeps it.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();

        // The cutoff is "now" minus 30 days, kept at the current wall-clock
        // time. Date this article to that very same calendar day but at one
        // second past midnight — genuinely older than the cutoff instant,
        // yet on a string `<` the RFC 3339 `T` makes it look newer.
        let now = chrono::Utc::now();
        let cutoff_day = (now - chrono::Duration::days(30)).date_naive();
        let old = cutoff_day.and_hms_opt(0, 0, 1).unwrap().and_utc();
        // Skip the rare case where the test runs within a second of midnight
        // and the article is not actually before the cutoff.
        assert!(old < now - chrono::Duration::days(30));
        insert_read_article_published(&conn, feed_id, "old", &old.to_rfc3339());
        // Comfortably inside the window — must be kept.
        let recent = chrono::Utc::now() - chrono::Duration::days(1);
        insert_read_article_published(&conn, feed_id, "recent", &recent.to_rfc3339());

        let removed = cleanup_old_articles(&conn, 30).unwrap();
        assert_eq!(removed, 1, "exactly the past-cutoff article should go");

        let surviving: Vec<String> = conn
            .prepare("SELECT guid FROM articles WHERE guid IN ('old','recent')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(surviving, ["recent"], "the old article must be deleted");
    }

    #[test]
    fn cleanup_keeps_starred_and_read_later_articles() {
        let (conn, _fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        insert_read_article_published(&conn, feed_id, "starred", &old);
        insert_read_article_published(&conn, feed_id, "later", &old);
        conn.execute("UPDATE articles SET is_starred = 1 WHERE guid = 'starred'", [])
            .unwrap();
        conn.execute("UPDATE articles SET read_later = 1 WHERE guid = 'later'", [])
            .unwrap();

        cleanup_old_articles(&conn, 30).unwrap();
        let kept: i64 = conn
            .query_row(
                "SELECT count(*) FROM articles WHERE guid IN ('starred','later')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 2, "starred / read-later articles are never purged");
    }

    #[test]
    fn cleanup_keeps_highlighted_articles() {
        // A read article the user has highlighted must survive retention: the
        // highlights cascade-delete with the article, so purging it would
        // silently destroy the user's annotations. An unhighlighted read
        // article of the same age is still purged.
        let (conn, _fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        insert_read_article_published(&conn, feed_id, "annotated", &old);
        insert_read_article_published(&conn, feed_id, "plain", &old);

        let annotated_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'annotated'", [], |r| {
                r.get(0)
            })
            .unwrap();
        insert_highlight(
            &conn,
            &hl(annotated_id, "kept quote", "", "", 0, "yellow", ""),
        )
        .unwrap();

        let removed = cleanup_old_articles(&conn, 30).unwrap();
        assert_eq!(removed, 1, "only the unhighlighted read article is purged");

        let surviving: Vec<String> = conn
            .prepare("SELECT guid FROM articles WHERE guid IN ('annotated','plain')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(surviving, ["annotated"], "the highlighted article is kept");
        // And its highlights are intact, not cascade-deleted.
        assert_eq!(list_highlights(&conn, annotated_id).unwrap().len(), 1);
    }

    #[test]
    fn cleanup_with_non_positive_days_is_a_no_op() {
        // A retention window of 0 days builds the modifier `'-0 days'`, whose
        // cutoff `datetime('now', '-0 days')` is *now* — left unguarded the
        // DELETE would purge every read article regardless of age. A negative
        // window builds a malformed `'--N days'` modifier. Both must be
        // rejected as no-ops so a bad caller / corrupt setting cannot trigger
        // a mass deletion.
        let (conn, _fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        // A read article published just now — well inside any sane window,
        // yet `datetime('now', '-0 days')` would still sweep it.
        let now = chrono::Utc::now().to_rfc3339();
        insert_read_article_published(&conn, feed_id, "fresh", &now);

        assert_eq!(cleanup_old_articles(&conn, 0).unwrap(), 0, "0 days deletes nothing");
        assert_eq!(cleanup_old_articles(&conn, -30).unwrap(), 0, "negative days deletes nothing");

        let kept: i64 = conn
            .query_row(
                "SELECT count(*) FROM articles WHERE guid = 'fresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "the fresh article survives a non-positive window");
    }

    /// Count `articles` rows carrying a given `(feed_id, guid)` — 0 or 1 given
    /// the `UNIQUE(feed_id, guid)` constraint.
    fn guid_count(conn: &Connection, feed_id: i64, guid: &str) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM articles WHERE feed_id = ?1 AND guid = ?2",
            params![feed_id, guid],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn retention_tombstone_blocks_reingestion_of_purged_article() {
        // The #98 loop: a full-archive feed (Hugo's `index.xml` ships its entire
        // history) keeps every past item in the document, so a read article that
        // retention purges is re-fetched on the very next refresh. Without a
        // tombstone it lands again as fresh *unread*, resurfacing the whole
        // archive the user just cleared — every day the daily cleanup runs. The
        // purge records a tombstone so the re-fetch is dropped.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();

        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        insert_read_article_published(&conn, feed_id, "archived", &old);

        assert_eq!(cleanup_old_articles(&conn, 30).unwrap(), 1);
        assert_eq!(guid_count(&conn, feed_id, "archived"), 0, "purged by retention");

        // The next refresh re-fetches the still-in-feed item. Pass dedup=false
        // so only the tombstone (not the soft URL check) can suppress it — the
        // purged row's URL is free again, so the unique index alone would not
        // block a re-insert.
        let refetched = NewArticle {
            guid: "archived".into(),
            url: Some("https://example.com/archived".into()),
            title: "Archived".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: Some(old),
            enclosures: Vec::new(),
        };
        assert!(
            !upsert_article(&conn, feed_id, &refetched, false, &[]).unwrap(),
            "a retention-purged article must not be re-ingested as new"
        );
        assert_eq!(
            guid_count(&conn, feed_id, "archived"),
            0,
            "the purged article stays gone across refreshes"
        );
    }

    #[test]
    fn tombstone_is_scoped_to_its_feed() {
        // The tombstone key is (feed_id, guid). A different feed carrying the
        // same guid is unaffected — its article ingests normally.
        let (conn, fixture) = test_db();
        let feed_a: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        let feed_b = insert_feed(
            &conn,
            "https://other.example/feed.xml",
            None,
            "Other",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();

        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        insert_read_article_published(&conn, feed_a, "shared", &old);
        assert_eq!(cleanup_old_articles(&conn, 30).unwrap(), 1);

        let a = NewArticle {
            guid: "shared".into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: Some(old),
            enclosures: Vec::new(),
        };
        assert!(
            upsert_article(&conn, feed_b, &a, false, &[]).unwrap(),
            "the same guid under a different feed is not tombstoned"
        );
    }

    // ── smart dedupe (cross-feed URL first-win) ──────────────────────

    fn url_count(conn: &Connection, url: &str) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM articles WHERE url = ?1",
            params![url],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn upsert_article_url_dedup_keeps_first_feed_across_feeds() {
        // Two subscriptions push the same story link. Smart dedupe keeps the
        // first-fetched row (and its feed_id); the later feed's insert is a
        // no-op — no second row, no reassignment.
        let (conn, fixture) = test_db();
        let feed_a: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        let feed_b = insert_feed(
            &conn,
            "https://other.example/feed.xml",
            None,
            "Other",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();

        let shared = "https://example.com/shared-story";
        let first = NewArticle {
            guid: "guid-a".into(),
            url: Some(shared.into()),
            title: "First win".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "from A".into(),
            image_url: None,
            published_at: Some("2026-01-01T00:00:00+00:00".into()),
            enclosures: Vec::new(),
        };
        let second = NewArticle {
            guid: "guid-b".into(),
            url: Some(shared.into()),
            title: "Later feed copy".into(),
            author: None,
            summary: Some("newer summary".into()),
            content_html: None,
            body_text: "from B".into(),
            image_url: None,
            published_at: Some("2026-06-01T00:00:00+00:00".into()),
            enclosures: Vec::new(),
        };

        assert!(upsert_article(&conn, feed_a, &first, true, &[]).unwrap());
        assert!(
            !upsert_article(&conn, feed_b, &second, true, &[]).unwrap(),
            "same URL from another feed must not insert as new"
        );
        assert_eq!(url_count(&conn, shared), 1, "exactly one row per URL");

        let (kept_feed, kept_title, kept_guid): (i64, String, String) = conn
            .query_row(
                "SELECT feed_id, title, guid FROM articles WHERE url = ?1",
                params![shared],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kept_feed, feed_a, "first-fetched feed keeps the row");
        assert_eq!(kept_title, "First win");
        assert_eq!(kept_guid, "guid-a");
    }

    #[test]
    fn upsert_article_allows_multiple_empty_urls() {
        // The partial unique index excludes NULL/empty URLs so guid-less or
        // link-less items can still coexist.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        let mk = |guid: &str| NewArticle {
            guid: guid.into(),
            url: None,
            title: guid.into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(upsert_article(&conn, feed_id, &mk("empty-1"), true, &[]).unwrap());
        assert!(upsert_article(&conn, feed_id, &mk("empty-2"), true, &[]).unwrap());
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM articles WHERE url IS NULL AND guid LIKE 'empty-%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn upsert_article_url_dedup_even_when_setting_flag_off() {
        // Unique index is the backstop: even if a caller passes dedup=false
        // (legacy newsletter path), a second feed cannot create a duplicate URL.
        let (conn, fixture) = test_db();
        let feed_a: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        let feed_b = insert_feed(
            &conn,
            "https://b.example/feed.xml",
            None,
            "B",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let url = "https://example.com/always-unique";
        let a = NewArticle {
            guid: "a".into(),
            url: Some(url.into()),
            title: "A".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        let b = NewArticle {
            guid: "b".into(),
            url: Some(url.into()),
            title: "B".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(upsert_article(&conn, feed_a, &a, false, &[]).unwrap());
        assert!(!upsert_article(&conn, feed_b, &b, false, &[]).unwrap());
        assert_eq!(url_count(&conn, url), 1);
    }

    #[test]
    fn migration_collapses_duplicate_urls_keeping_earliest() {
        // Apply migrations through v28 (pre–URL-unique), plant two rows with
        // the same URL, then finish v29 and confirm the later id is gone.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        // v28 is the migration before URL uniqueness (0-based index 27).
        MIGRATIONS.to_version(&mut conn, 28).unwrap();
        register_functions(&conn).unwrap();
        conn.execute(
            "INSERT INTO feeds(id, feed_url, title, source_type)
             VALUES (1, 'https://a.example/feed', 'A', 'rss'),
                    (2, 'https://b.example/feed', 'B', 'rss')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO articles(id, feed_id, guid, url, title, body_text, fetched_at)
             VALUES (10, 1, 'g-early', 'https://example.com/dup', 'Early', '', '2026-01-01 00:00:00'),
                    (20, 2, 'g-late',  'https://example.com/dup', 'Late',  '', '2026-02-01 00:00:00')",
            [],
        )
        .unwrap();
        assert_eq!(url_count(&conn, "https://example.com/dup"), 2);

        MIGRATIONS.to_latest(&mut conn).unwrap();
        assert_eq!(url_count(&conn, "https://example.com/dup"), 1);
        let kept: (i64, String) = conn
            .query_row(
                "SELECT id, title FROM articles WHERE url = 'https://example.com/dup'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kept, (10, "Early".into()));
        let flag = setting_flag(&conn, "dedup_enabled", false);
        assert!(flag, "migration seeds dedup_enabled=1 when unset");
    }

    // ── article-list chronological ordering ──────────────────────────

    #[test]
    fn list_articles_orders_mixed_date_formats_by_real_instant() {
        // The newest-first list sorts on COALESCE(published_at, fetched_at).
        // A feed-dated row carries `published_at` as RFC 3339 (`...T...+00:00`,
        // a `T` separator); a dateless row falls through to `fetched_at`, which
        // SQLite stores space-separated (`... ...`). A raw string `<` compares
        // the two formats byte-for-byte and the `T` (0x54) sorts *after* a
        // space (0x20), so a dated row looks up to a day newer than it is —
        // a list mixing both kinds of rows comes out subtly out of order.
        //
        // This pins that exact mix: a dateless row fetched *later* than a
        // dated row was published. The dateless one must sort first. Under a
        // string compare the dated row wins (the `T`); `datetime()` fixes it.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        // Drop the bare fixture article so only the two controlled rows remain.
        conn.execute("DELETE FROM articles WHERE id = ?1", [fixture])
            .unwrap();

        // Same calendar day so the format difference — not the date — decides:
        // the dated row published at 10:00, the dateless row fetched at 12:00.
        // '2024-01-15T10:00:00+00:00' > '2024-01-15 12:00:00' as raw strings
        // (T beats space) but the dateless row is the genuinely newer one.
        insert_read_article_published(&conn, feed_id, "dated", "2024-01-15T10:00:00+00:00");
        let dateless = NewArticle {
            guid: "dateless".into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &dateless, false, &[]).unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2024-01-15 12:00:00' WHERE guid = 'dateless'",
            [],
        )
        .unwrap();

        // Newest-first: the dateless row (fetched 12:00) precedes the dated
        // row (published 10:00).
        let newest = list_articles(&conn, &ArticleQuery::All, false, None, false, 50, 0).unwrap();
        let newest_guids: Vec<i64> = newest.iter().map(|a| a.id).collect();
        let dated_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'dated'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let dateless_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'dateless'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            newest_guids,
            [dateless_id, dated_id],
            "newest-first: the later-fetched dateless article must come first"
        );

        // Oldest-first is the exact mirror.
        let oldest = list_articles(&conn, &ArticleQuery::All, false, None, true, 50, 0).unwrap();
        let oldest_guids: Vec<i64> = oldest.iter().map(|a| a.id).collect();
        assert_eq!(
            oldest_guids,
            [dated_id, dateless_id],
            "oldest-first: the earlier-published dated article must come first"
        );
    }

    #[test]
    fn list_articles_search_defaults_to_relevance_then_date() {
        // Active search orders by FTS rank first; equal-rank ties break by date.
        // `sort_by_relevance: false` restores pure chronological order.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute("DELETE FROM articles WHERE id = ?1", [fixture])
            .unwrap();

        let older = NewArticle {
            guid: "china-old".into(),
            url: None,
            title: "China older hit".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "about China".into(),
            image_url: None,
            published_at: Some("2025-07-29T12:00:00+00:00".into()),
            enclosures: Vec::new(),
        };
        let newer = NewArticle {
            guid: "china-new".into(),
            url: None,
            title: "China newer hit".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "about China".into(),
            image_url: None,
            published_at: Some("2026-05-12T12:00:00+00:00".into()),
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &older, false, &[]).unwrap();
        upsert_article(&conn, feed_id, &newer, false, &[]).unwrap();
        let older_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'china-old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let newer_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'china-new'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // Same FTS score → secondary date DESC (newest first).
        let by_rank = list_articles(
            &conn,
            &ArticleQuery::All,
            false,
            Some("China"),
            false,
            50,
            0,
        )
        .unwrap();
        assert_eq!(
            by_rank.iter().map(|a| a.id).collect::<Vec<_>>(),
            [newer_id, older_id],
            "relevance sort uses date as secondary (newest)"
        );

        let by_date_oldest = list_articles_sorted(
            &conn,
            &ArticleQuery::All,
            false,
            Some("China"),
            true,
            false,
            50,
            0,
        )
        .unwrap();
        assert_eq!(
            by_date_oldest.iter().map(|a| a.id).collect::<Vec<_>>(),
            [older_id, newer_id],
            "sort_by_relevance=false must use chronological oldest-first"
        );
    }

    #[test]
    fn list_articles_strict_and_or_not_semantics() {
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute("DELETE FROM articles WHERE id = ?1", [fixture])
            .unwrap();

        add_article(
            &conn,
            feed_id,
            "t1",
            "Trump visits China",
            "trade talks continue",
        );
        add_article(
            &conn,
            feed_id,
            "t2",
            "Biden on China",
            "diplomacy notes",
        );
        add_article(
            &conn,
            feed_id,
            "t3",
            "Trump tariff plan",
            "tariff announcement",
        );
        add_article(
            &conn,
            feed_id,
            "t4",
            "Opinion: markets",
            "Trump China opinion piece",
        );

        let and_hits = list_articles(
            &conn,
            &ArticleQuery::All,
            false,
            Some("Trump china"),
            false,
            50,
            0,
        )
        .unwrap();
        let and_titles: Vec<&str> = and_hits.iter().map(|a| a.title.as_str()).collect();
        assert!(and_titles.iter().any(|t| t.contains("visits China")));
        assert!(
            !and_titles.iter().any(|t| t.contains("tariff plan")),
            "AND must exclude Trump-only without china: {and_titles:?}"
        );

        let or_hits = list_articles(
            &conn,
            &ArticleQuery::All,
            false,
            Some("Trump OR Biden"),
            false,
            50,
            0,
        )
        .unwrap();
        assert!(or_hits.len() >= 3);

        let not_hits = list_articles(
            &conn,
            &ArticleQuery::All,
            false,
            Some("Trump -tariff"),
            false,
            50,
            0,
        )
        .unwrap();
        let not_titles: Vec<&str> = not_hits.iter().map(|a| a.title.as_str()).collect();
        assert!(
            !not_titles.iter().any(|t| t.to_lowercase().contains("tariff")),
            "NOT must exclude tariff: {not_titles:?}"
        );

        let grouped = list_articles(
            &conn,
            &ArticleQuery::All,
            false,
            Some("(Trump OR Biden) china"),
            false,
            50,
            0,
        )
        .unwrap();
        assert!(!grouped.is_empty());

        let phrase = list_articles(
            &conn,
            &ArticleQuery::All,
            false,
            Some("\"visits China\""),
            false,
            50,
            0,
        )
        .unwrap();
        assert_eq!(phrase.len(), 1);
        assert!(phrase[0].title.contains("visits China"));
    }

    // ── feed rename vs. refresh ──────────────────────────────────────

    #[test]
    fn manual_rename_survives_a_metadata_refresh() {
        // A user renames a feed; a later refresh pulls the feed document's own
        // `<title>` through `update_feed_meta`. The rename must stick — only
        // the other metadata (site_url, description, favicon) should update.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        rename_feed(&conn, feed_id, "My Custom Name").unwrap();

        // Simulate a refresh: the feed document still calls itself "Example Feed".
        update_feed_meta(
            &conn,
            feed_id,
            Some("Example Feed"),
            Some("https://example.com"),
            Some("A description"),
            None,
        )
        .unwrap();

        let (title, site_url): (String, Option<String>) = conn
            .query_row("SELECT title, site_url FROM feeds WHERE id = ?1", [feed_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            title, "My Custom Name",
            "a manual rename must not be reverted by a refresh"
        );
        assert_eq!(
            site_url.as_deref(),
            Some("https://example.com"),
            "non-title metadata must still refresh normally"
        );
    }

    #[test]
    fn rename_feed_rejects_an_empty_title() {
        // An empty (or whitespace-only) rename must be refused: a rename sets
        // `custom_title = 1`, so an empty title would blank the feed in the
        // sidebar forever — `update_feed_meta` can no longer restore it. The
        // original title must be left untouched.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let original: String = conn
            .query_row("SELECT title FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))
            .unwrap();

        for blank in ["", "   ", "\t\n"] {
            let err = rename_feed(&conn, feed_id, blank).unwrap_err();
            assert!(
                err.to_string().contains("emptyFeedTitle"),
                "blank rename {blank:?} should be rejected, got: {err}"
            );
        }

        let (title, custom): (String, bool) = conn
            .query_row(
                "SELECT title, custom_title FROM feeds WHERE id = ?1",
                [feed_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, original, "a rejected rename must not alter the title");
        assert!(!custom, "a rejected rename must not set the custom_title flag");
    }

    #[test]
    fn rename_feed_trims_surrounding_whitespace() {
        // A padded name (`"  News  "`) is stored trimmed — the db function is
        // the single trimming chokepoint, so the command no longer trims.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        rename_feed(&conn, feed_id, "  Tech News  ").unwrap();
        let title: String = conn
            .query_row("SELECT title FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Tech News");
    }

    #[test]
    fn refresh_updates_title_when_not_renamed() {
        // A feed the user has never renamed should still pick up the feed
        // document's title on refresh — the guard only protects manual names.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        update_feed_meta(&conn, feed_id, Some("Renamed Upstream"), None, None, None).unwrap();

        let title: String = conn
            .query_row("SELECT title FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Renamed Upstream");
    }

    #[test]
    fn refresh_with_empty_title_keeps_existing_name() {
        // `feed-rs` parses `<title></title>` (or a stray empty title) as
        // `Some("")`, and the scheduler's refresh path forwards it straight
        // into `update_feed_meta`. An empty string must be treated like
        // `None` — the feed's good sidebar name must survive, not be wiped
        // blank. The other metadata columns get the same empty-string guard.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        // Seed real metadata first (the feed has never been renamed by hand).
        update_feed_meta(
            &conn,
            feed_id,
            Some("Good Feed Name"),
            Some("https://example.com"),
            Some("A description"),
            Some("https://example.com/favicon.ico"),
        )
        .unwrap();

        // A later refresh serves empty metadata — every field must be ignored.
        update_feed_meta(
            &conn,
            feed_id,
            Some(""),
            Some(""),
            Some(""),
            Some(""),
        )
        .unwrap();

        let (title, site_url, description, favicon): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT title, site_url, description, favicon_url FROM feeds WHERE id = ?1",
                [feed_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            title, "Good Feed Name",
            "an empty <title> on refresh must not blank the sidebar name"
        );
        assert_eq!(site_url.as_deref(), Some("https://example.com"));
        assert_eq!(description.as_deref(), Some("A description"));
        assert_eq!(favicon.as_deref(), Some("https://example.com/favicon.ico"));
    }

    #[test]
    fn rename_tag_rejects_collision_with_another_tag() {
        let (conn, _aid) = test_db();
        let rust = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        let go = create_tag(&conn, "Go", TAG_KIND_INTEREST).unwrap();

        // Renaming "Go" onto an exact match of "Rust" must be rejected with the
        // localisable code rather than the raw UNIQUE-constraint SQLite error.
        let err = rename_tag(&conn, go, "Rust").unwrap_err();
        assert!(
            matches!(err, AppError::Coded("tagNameExists")),
            "expected tagNameExists, got {err:?}"
        );
        // A *case variant* of another tag must be rejected too — otherwise it
        // would create the near-duplicate `create_tag` deliberately collapses.
        let err = rename_tag(&conn, go, "rust").unwrap_err();
        assert!(matches!(err, AppError::Coded("tagNameExists")));

        // The clash check did not corrupt either name.
        let name = |id| {
            conn.query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(name(rust), "Rust");
        assert_eq!(name(go), "Go");
    }

    #[test]
    fn rename_tag_allows_genuine_rename_and_self_case_change() {
        let (conn, _aid) = test_db();
        let id = create_tag(&conn, "draft", TAG_KIND_INTEREST).unwrap();
        // A free name is accepted.
        rename_tag(&conn, id, "Reading").unwrap();
        // Re-casing the tag's *own* name is allowed (no other tag clashes).
        rename_tag(&conn, id, "READING").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "READING");
    }

    // --- folders: same name-uniqueness family as tags. `folders.name` has no
    //     UNIQUE constraint, so create/rename must dedup in code. ---

    #[test]
    fn create_folder_is_idempotent_on_name() {
        let (conn, _aid) = test_db();
        let first = create_folder(&conn, "Tech").unwrap();
        // Re-creating the same name returns the existing id, not a second row.
        assert_eq!(create_folder(&conn, "Tech").unwrap(), first);
        // Case-insensitive: "tech" resolves to the same folder as "Tech".
        assert_eq!(create_folder(&conn, "tech").unwrap(), first);
        assert_eq!(list_folders(&conn).unwrap().len(), 1);
    }

    #[test]
    fn folder_id_by_name_reuses_existing_folder_case_insensitively() {
        // OPML import attaches feeds via `folder_id_by_name`; an imported
        // folder whose name matches an existing one (in any case) must reuse
        // that folder rather than spawn a near-duplicate.
        let (conn, _aid) = test_db();
        let existing = create_folder(&conn, "News").unwrap();
        assert_eq!(folder_id_by_name(&conn, "news").unwrap(), existing);
        assert_eq!(list_folders(&conn).unwrap().len(), 1);
        // A genuinely new name still creates a folder.
        let fresh = folder_id_by_name(&conn, "Science").unwrap();
        assert_ne!(fresh, existing);
        assert_eq!(list_folders(&conn).unwrap().len(), 2);
    }

    #[test]
    fn rename_folder_rejects_collision_with_another_folder() {
        let (conn, _aid) = test_db();
        let tech = create_folder(&conn, "Tech").unwrap();
        let news = create_folder(&conn, "News").unwrap();

        // Renaming "News" onto an exact match of "Tech" is rejected with the
        // localisable code.
        let err = rename_folder(&conn, news, "Tech").unwrap_err();
        assert!(
            matches!(err, AppError::Coded("folderNameExists")),
            "expected folderNameExists, got {err:?}"
        );
        // A case variant of another folder is rejected too.
        let err = rename_folder(&conn, news, "tech").unwrap_err();
        assert!(matches!(err, AppError::Coded("folderNameExists")));

        // Neither name was corrupted by the clash check.
        let name = |id| {
            conn.query_row("SELECT name FROM folders WHERE id = ?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(name(tech), "Tech");
        assert_eq!(name(news), "News");
    }

    #[test]
    fn rename_folder_allows_genuine_rename_and_self_case_change() {
        let (conn, _aid) = test_db();
        let id = create_folder(&conn, "Misc").unwrap();
        // A free name is accepted.
        rename_folder(&conn, id, "Reading").unwrap();
        // Re-casing the folder's *own* name is allowed (no other folder clashes).
        rename_folder(&conn, id, "READING").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "READING");
    }

    #[test]
    fn create_folder_trims_whitespace_and_dedups_padded_names() {
        // An OPML-imported folder name carrying surrounding whitespace
        // (`<outline text=" Tech ">`) must resolve to the same folder as a
        // plain "Tech" — without the trim the `COLLATE NOCASE` lookup misses
        // it and a second, visually identical folder is spawned.
        let (conn, _aid) = test_db();
        let tech = create_folder(&conn, "Tech").unwrap();
        assert_eq!(create_folder(&conn, "  Tech  ").unwrap(), tech);
        assert_eq!(create_folder(&conn, "\tTech\n").unwrap(), tech);
        // The first creation also stores the trimmed form, not the padded one.
        let padded = create_folder(&conn, " News ").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = ?1", [padded], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "News");
        assert_eq!(list_folders(&conn).unwrap().len(), 2);
    }

    #[test]
    fn rename_folder_rejects_whitespace_padded_collision() {
        // A rename to a whitespace-padded variant of another folder's name
        // must still be rejected — the trim makes the clash check see through
        // the padding instead of letting the near-duplicate slip past.
        let (conn, _aid) = test_db();
        let _tech = create_folder(&conn, "Tech").unwrap();
        let news = create_folder(&conn, "News").unwrap();
        let err = rename_folder(&conn, news, "  Tech  ").unwrap_err();
        assert!(matches!(err, AppError::Coded("folderNameExists")));
    }

    #[test]
    fn create_folder_rejects_an_empty_or_blank_name() {
        // An empty / whitespace-only name must be refused at the DB chokepoint:
        // `import_opml` reaches `create_folder` through `folder_id_by_name`
        // without the `PromptDialog` guard, so a blank label would otherwise
        // insert an unlabelled folder into the sidebar. No row may be created.
        let (conn, _aid) = test_db();
        for blank in ["", "   ", "\t\n"] {
            let err = create_folder(&conn, blank).unwrap_err();
            assert!(
                matches!(err, AppError::Coded("emptyFolderName")),
                "blank name {blank:?} must be rejected"
            );
        }
        assert!(list_folders(&conn).unwrap().is_empty());
    }

    #[test]
    fn rename_folder_rejects_an_empty_or_blank_name() {
        // Renaming a folder to a blank string would leave it unlabelled with
        // no recovery path — rejected the same way `create_folder` is.
        let (conn, _aid) = test_db();
        let tech = create_folder(&conn, "Tech").unwrap();
        for blank in ["", "   ", "\t\n"] {
            let err = rename_folder(&conn, tech, blank).unwrap_err();
            assert!(matches!(err, AppError::Coded("emptyFolderName")));
        }
        // The folder keeps its original name.
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = ?1", [tech], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Tech");
    }

    #[test]
    fn daily_article_counts_uses_fetched_at_not_published_at() {
        // Heatmap "收录" is ingestion time. An old-published article fetched
        // today must count; a today-published article fetched long ago must not.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute("DELETE FROM articles WHERE id = ?1", [fixture])
            .unwrap();

        let old_pub = NewArticle {
            guid: "old-pub-new-fetch".into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: Some("2020-01-01T00:00:00+00:00".into()),
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &old_pub, false, &[]).unwrap();

        let new_pub = NewArticle {
            guid: "new-pub-old-fetch".into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: Some("2026-08-01T00:00:00+00:00".into()),
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &new_pub, false, &[]).unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = datetime('now', '-60 days')
             WHERE guid = 'new-pub-old-fetch'",
            [],
        )
        .unwrap();

        let rows = daily_article_counts(&conn, 30).unwrap();
        let total: i64 = rows.iter().map(|(_, c)| c).sum();
        assert_eq!(total, 1, "only the recently-fetched row should count: {rows:?}");
        let guids_today: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM articles
                 WHERE guid = 'old-pub-new-fetch'
                   AND datetime(fetched_at, 'localtime')
                       >= datetime('now', 'localtime', 'start of day', '-29 days')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(guids_today, 1);
    }

    #[test]
    fn clear_all_data_wipes_feeds_folders_tags_and_rules() {
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        // Seed enough FTS content that a plain DELETE would leave measurable
        // shadow-table residue; rebuild should collapse it.
        for i in 0..50 {
            let article = NewArticle {
                guid: format!("fts-bloat-{i}"),
                url: None,
                title: format!("Title with searchable words {i}"),
                author: None,
                summary: None,
                content_html: None,
                body_text: format!("Body text for full-text search document number {i}"),
                image_url: None,
                published_at: None,
                enclosures: Vec::new(),
            };
            upsert_article(&conn, feed_id, &article, false, &[]).unwrap();
        }
        create_folder(&conn, "News").unwrap();
        create_tag(&conn, "tech", TAG_KIND_INTEREST).unwrap();
        // Global rule — would survive DELETE FROM feeds without an explicit wipe.
        create_rule(&conn, "skip ads", None, "title", "sponsored", "skip").unwrap();

        clear_all_data(&conn).unwrap();

        let counts: (i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM feeds),
                    (SELECT COUNT(*) FROM folders),
                    (SELECT COUNT(*) FROM articles),
                    (SELECT COUNT(*) FROM tags),
                    (SELECT COUNT(*) FROM rules)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0, 0, 0, 0));

        let fts_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_rows, 0, "virtual FTS table must be empty after clear");

        // FTS5 shadow segments must be rebuilt empty — a plain DELETE leaves
        // tombstones in `_data` / `_idx` that still occupy disk.
        let fts_data: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles_fts_data", [], |r| r.get(0))
            .unwrap();
        assert!(
            fts_data <= 2,
            "rebuild should collapse FTS shadow segments, got articles_fts_data={fts_data}"
        );

        // Config metadata row is required by FTS5 and must remain.
        let fts_config: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles_fts_config", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_config, 1);
    }

    #[test]
    fn ai_usage_records_only_nonzero_rows_and_aggregates() {
        use crate::ai::TokenUsage;

        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_latest(&mut conn).unwrap();

        // Zero-token calls (machine translation, providers without usage) are
        // skipped so the ledger stays a meaningful LLM accounting table.
        record_ai_usage(
            &conn,
            "translate",
            "",
            "",
            TokenUsage::default(),
        )
        .unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);

        record_ai_usage(
            &conn,
            "summarize",
            "deepseek",
            "deepseek-v4-flash",
            TokenUsage {
                prompt_tokens: 120,
                completion_tokens: 45,
                reasoning_tokens: 30,
                cache_hit_tokens: 80,
            },
        )
        .unwrap();
        record_ai_usage(
            &conn,
            "ask",
            "deepseek",
            "deepseek-v4-flash",
            TokenUsage {
                prompt_tokens: 300,
                completion_tokens: 80,
                reasoning_tokens: 50,
                cache_hit_tokens: 0,
            },
        )
        .unwrap();
        record_ai_usage(
            &conn,
            "summarize",
            "deepseek",
            "deepseek-v4-flash",
            TokenUsage {
                prompt_tokens: 90,
                completion_tokens: 20,
                reasoning_tokens: 0,
                cache_hit_tokens: 40,
            },
        )
        .unwrap();

        let stats = ai_usage_stats(&conn, 30).unwrap();
        assert_eq!(stats.by_feature.len(), 2);
        let summarize = &stats.by_feature[0];
        assert_eq!(summarize.feature, "ask");
        let summarize = &stats.by_feature[1];
        assert_eq!(summarize.feature, "summarize");
        assert_eq!(summarize.calls, 2);
        assert_eq!(summarize.prompt_tokens, 210);
        assert_eq!(summarize.completion_tokens, 65);
        assert_eq!(summarize.reasoning_tokens, 30);
        assert_eq!(summarize.cache_hit_tokens, 120);

        let t = &stats.total;
        assert_eq!(t.calls, 3);
        assert_eq!(t.prompt_tokens, 510);
        assert_eq!(t.completion_tokens, 145);
        assert_eq!(t.reasoning_tokens, 80);
        assert_eq!(t.cache_hit_tokens, 120);

        // DeepSeek V4 Flash defaults: hit 0.02 + miss 1 + out 2 (CNY / M).
        // hit=120, miss=390, out=145 → 0.0000024 + 0.00039 + 0.00029 = 0.0006824
        let cost = estimate_ai_cost_cny(&conn, &stats);
        assert!((cost - 0.0006824).abs() < 1e-12);

        // Rows outside the window are excluded.
        conn.execute(
            "UPDATE ai_usage SET created_at = datetime('now', '-60 days') WHERE feature = 'ask'",
            [],
        )
        .unwrap();
        let windowed = ai_usage_stats(&conn, 30).unwrap();
        assert_eq!(windowed.total.calls, 2);
        assert_eq!(windowed.total.prompt_tokens, 210);
    }

    #[test]
    fn tag_aliases_migration_additive_and_crud() {
        // v30 only adds `tag_aliases` — existing tags / article_tags survive.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        MIGRATIONS.to_version(&mut conn, 29).unwrap();
        register_functions(&conn).unwrap();
        conn.execute(
            "INSERT INTO feeds(id, feed_url, title, source_type)
             VALUES (1, 'https://a.example/feed', 'A', 'rss')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO articles(id, feed_id, guid, url, title, body_text)
             VALUES (1, 1, 'g1', 'https://example.com/a', 'T', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tags(id, name, color, position, kind)
             VALUES (10, 'Rust', 'clay', 0, 'interest')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO article_tags(article_id, tag_id) VALUES (1, 10)",
            [],
        )
        .unwrap();

        MIGRATIONS.to_latest(&mut conn).unwrap();

        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        let link_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM article_tags", [], |r| r.get(0))
            .unwrap();
        let alias_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tag_aliases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tag_count, 1);
        assert_eq!(link_count, 1);
        assert_eq!(alias_count, 0, "v1 ships with an empty alias table");

        let alias_id = create_tag_alias(&conn, 10, "  rustlang  ").unwrap();
        let listed = list_tag_aliases(&conn, Some(10), Some(TAG_KIND_INTEREST)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, alias_id);
        assert_eq!(listed[0].alias, "rustlang");
        assert_eq!(listed[0].tag_name, "Rust");

        assert_eq!(
            resolve_tag_by_name_or_alias(&conn, TAG_KIND_INTEREST, "RustLang")
                .unwrap(),
            Some(10)
        );
        assert_eq!(
            resolve_tag_by_name_or_alias(&conn, TAG_KIND_INTEREST, "rust").unwrap(),
            Some(10)
        );
    }

    #[test]
    fn create_tag_alias_rejects_empty_and_tag_name_clash() {
        let (conn, _) = test_db();
        let rust = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        let _go = create_tag(&conn, "Go", TAG_KIND_INTEREST).unwrap();

        assert!(matches!(
            create_tag_alias(&conn, rust, "   ").unwrap_err(),
            AppError::Coded("emptyTagAlias")
        ));
        // Alias must not equal any tag name in the same kind (incl. self).
        assert!(matches!(
            create_tag_alias(&conn, rust, "Go").unwrap_err(),
            AppError::Coded("tagAliasConflictsWithTagName")
        ));
        assert!(matches!(
            create_tag_alias(&conn, rust, "rust").unwrap_err(),
            AppError::Coded("tagAliasConflictsWithTagName")
        ));

        create_tag_alias(&conn, rust, "rustlang").unwrap();
        assert!(matches!(
            create_tag_alias(&conn, rust, "RustLang").unwrap_err(),
            AppError::Coded("tagAliasExists")
        ));
    }

    #[test]
    fn delete_tag_cascades_aliases() {
        let (conn, _) = test_db();
        let rust = create_tag(&conn, "Rust", TAG_KIND_INTEREST).unwrap();
        create_tag_alias(&conn, rust, "rustlang").unwrap();
        create_tag_alias(&conn, rust, "rust-rs").unwrap();
        assert_eq!(list_tag_aliases(&conn, Some(rust), None).unwrap().len(), 2);

        delete_tag(&conn, rust).unwrap();
        assert!(list_tag_aliases(&conn, None, Some(TAG_KIND_INTEREST))
            .unwrap()
            .is_empty());
        let leftover: i64 = conn
            .query_row("SELECT COUNT(*) FROM tag_aliases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leftover, 0);
    }
}
