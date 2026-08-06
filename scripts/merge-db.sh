#!/usr/bin/env bash
# Merge one SQLite RSS/papr DB into another (bulk ATTACH + SQL).
# Usage: ./scripts/merge-db.sh rss.db papr.db
set -euo pipefail

SRC="${1:-}"
DEST="${2:-}"
if [[ -z "$SRC" || -z "$DEST" ]]; then
  echo "Usage: $0 <source.db> <dest.db>" >&2
  exit 1
fi
if [[ ! -f "$SRC" ]]; then echo "Missing source: $SRC" >&2; exit 1; fi
if [[ ! -f "$DEST" ]]; then echo "Missing dest: $DEST" >&2; exit 1; fi

SRC=$(cd "$(dirname "$SRC")" && pwd)/$(basename "$SRC")
DEST=$(cd "$(dirname "$DEST")" && pwd)/$(basename "$DEST")

TS=$(date +%Y%m%d%H%M%S)
BAK="${DEST}.bak.${TS}"
echo "==> Backup dest -> $BAK"
cp -p "$DEST" "$BAK"

count() { sqlite3 "$DEST" "SELECT count(*) FROM $1;" 2>/dev/null || echo 0; }

echo "==> Before: articles=$(count articles) feeds=$(count feeds) folders=$(count folders) tags=$(count tags)"

SRC_HAS_ARTICLES=$(sqlite3 "$SRC" "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='articles';")
SRC_HAS_LEGACY=$(sqlite3 "$SRC" "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='article_states';")

# Article cols for papr→papr (intersection)
if [[ "$SRC_HAS_ARTICLES" -gt 0 ]]; then
  echo "==> Source looks like papr schema (articles/feeds/folders)"
  sqlite3 "$DEST" \
    -cmd ".timeout 5000" \
    "ATTACH DATABASE '$SRC' AS src;" \
    "
PRAGMA foreign_keys=OFF;
BEGIN;

SELECT 'step: map folders' AS progress;
CREATE TEMP TABLE folder_map (src_id INTEGER PRIMARY KEY, dest_id INTEGER NOT NULL);
INSERT INTO folder_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.folders s JOIN folders d ON d.name = s.name;

SELECT 'step: insert missing folders' AS progress;
INSERT INTO folders(name, position)
  SELECT s.name, COALESCE(s.position, 0)
  FROM src.folders s
  WHERE NOT EXISTS (SELECT 1 FROM folders d WHERE d.name = s.name);

INSERT INTO folder_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.folders s JOIN folders d ON d.name = s.name
  WHERE s.id NOT IN (SELECT src_id FROM folder_map);

SELECT 'step: map feeds' AS progress;
CREATE TEMP TABLE feed_map (src_id INTEGER PRIMARY KEY, dest_id INTEGER NOT NULL);
INSERT INTO feed_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.feeds s JOIN feeds d ON d.feed_url = s.feed_url;

SELECT 'step: insert missing feeds' AS progress;
INSERT INTO feeds(feed_url, site_url, title, description, favicon_url, folder_id, source_type,
                  etag, last_modified, last_fetched_at, fetch_error, created_at)
  SELECT s.feed_url, s.site_url, s.title, s.description, s.favicon_url,
         (SELECT fm.dest_id FROM folder_map fm WHERE fm.src_id = s.folder_id),
         COALESCE(s.source_type, 'rss'),
         s.etag, s.last_modified, s.last_fetched_at, s.fetch_error,
         COALESCE(s.created_at, datetime('now'))
  FROM src.feeds s
  WHERE NOT EXISTS (SELECT 1 FROM feeds d WHERE d.feed_url = s.feed_url);

INSERT INTO feed_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.feeds s JOIN feeds d ON d.feed_url = s.feed_url
  WHERE s.id NOT IN (SELECT src_id FROM feed_map);

SELECT 'step: clear blocking tombstones' AS progress;
DELETE FROM article_tombstones
WHERE (feed_id, guid) IN (
  SELECT fm.dest_id, a.guid FROM src.articles a JOIN feed_map fm ON fm.src_id = a.feed_id
);

SELECT 'step: insert articles' AS progress;
INSERT OR IGNORE INTO articles($ART_COLS)
SELECT $(echo "$ART_COLS" | sed 's/feed_id/fm.dest_id AS feed_id/')
FROM src.articles a
JOIN feed_map fm ON fm.src_id = a.feed_id;

SELECT 'step: merge tags' AS progress;
CREATE TEMP TABLE tag_map (src_id INTEGER PRIMARY KEY, dest_id INTEGER NOT NULL);
INSERT INTO tags(name, color, position)
  SELECT s.name, COALESCE(s.color,'clay'), COALESCE(s.position,0)
  FROM src.tags s
  WHERE NOT EXISTS (SELECT 1 FROM tags d WHERE d.name = s.name);
INSERT INTO tag_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.tags s JOIN tags d ON d.name = s.name;

SELECT 'step: article_tags' AS progress;
INSERT OR IGNORE INTO article_tags(article_id, tag_id)
SELECT da.id, tm.dest_id
FROM src.article_tags sat
JOIN src.articles sa ON sa.id = sat.article_id
JOIN feed_map fm ON fm.src_id = sa.feed_id
JOIN articles da ON da.feed_id = fm.dest_id AND da.guid = sa.guid
JOIN tag_map tm ON tm.src_id = sat.tag_id;

SELECT 'step: highlights' AS progress;
INSERT INTO highlights(article_id, quote, prefix, suffix, text_offset, color, note, created_at)
SELECT da.id, h.quote, COALESCE(h.prefix,''), COALESCE(h.suffix,''),
       COALESCE(h.text_offset,0), COALESCE(h.color,'yellow'), COALESCE(h.note,''),
       COALESCE(h.created_at, datetime('now'))
FROM src.highlights h
JOIN src.articles sa ON sa.id = h.article_id
JOIN feed_map fm ON fm.src_id = sa.feed_id
JOIN articles da ON da.feed_id = fm.dest_id AND da.guid = sa.guid
WHERE EXISTS (SELECT 1 FROM sqlite_master WHERE name='highlights');

COMMIT;
SELECT 'step: fts rebuild' AS progress;
INSERT INTO articles_fts(articles_fts) VALUES('rebuild');
DETACH DATABASE src;
"

elif [[ "$SRC_HAS_LEGACY" -gt 0 ]]; then
  echo "==> Source is legacy rss.db (article_states / feeds.url)"
  sqlite3 "$DEST" \
    -cmd ".timeout 5000" \
    "
PRAGMA foreign_keys=OFF;
ATTACH DATABASE '$SRC' AS src;
BEGIN;

SELECT 'step: map feeds by url' AS progress;
CREATE TEMP TABLE feed_map (src_id TEXT PRIMARY KEY, dest_id INTEGER NOT NULL);
INSERT INTO feed_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.feeds s JOIN feeds d ON d.feed_url = s.url;

SELECT 'step: insert missing feeds' AS progress;
INSERT INTO feeds(feed_url, title, last_fetched_at, source_type, created_at)
  SELECT s.url, s.name,
         CASE WHEN s.last_fetched_at IS NOT NULL
              THEN datetime(s.last_fetched_at, 'unixepoch') END,
         'rss', datetime('now')
  FROM src.feeds s
  WHERE NOT EXISTS (SELECT 1 FROM feeds d WHERE d.feed_url = s.url);

INSERT INTO feed_map(src_id, dest_id)
  SELECT s.id, d.id FROM src.feeds s JOIN feeds d ON d.feed_url = s.url
  WHERE s.id NOT IN (SELECT src_id FROM feed_map);

-- Also map via article_states.feed_url when feed_id text differs
INSERT OR IGNORE INTO feed_map(src_id, dest_id)
  SELECT DISTINCT a.feed_id, d.id
  FROM src.article_states a
  JOIN feeds d ON d.feed_url = COALESCE(NULLIF(a.feed_url,''),
    (SELECT s.url FROM src.feeds s WHERE s.id = a.feed_id))
  WHERE a.feed_id IS NOT NULL
    AND a.feed_id NOT IN (SELECT src_id FROM feed_map);

SELECT 'step: clear blocking tombstones' AS progress;
DELETE FROM article_tombstones
WHERE (feed_id, guid) IN (
  SELECT fm.dest_id, a.article_id
  FROM src.article_states a
  JOIN feed_map fm ON fm.src_id = a.feed_id
);

SELECT 'step: insert articles from article_states' AS progress;
INSERT OR IGNORE INTO articles(
  feed_id, guid, url, title, author, summary, content_html, body_text,
  published_at, fetched_at, is_read, is_starred, read_later
)
SELECT
  fm.dest_id,
  a.article_id,
  a.link,
  COALESCE(NULLIF(a.title,''), '(untitled)'),
  a.author,
  a.summary,
  a.content,
  '',
  CASE
    WHEN a.pub_date IS NOT NULL AND a.pub_date != '' THEN a.pub_date
    WHEN a.pub_ts IS NOT NULL THEN datetime(a.pub_ts / 1000, 'unixepoch')
    ELSE NULL
  END,
  COALESCE(NULLIF(a.updated_at,''), datetime('now')),
  COALESCE(a.is_read, 0),
  COALESCE(a.is_starred, 0),
  0
FROM src.article_states a
JOIN feed_map fm ON fm.src_id = a.feed_id;

COMMIT;
SELECT 'step: fts rebuild (may take a while)' AS progress;
INSERT INTO articles_fts(articles_fts) VALUES('rebuild');
DETACH DATABASE src;
"
else
  echo "Source has neither articles nor article_states; aborting." >&2
  exit 1
fi

echo "==> After:  articles=$(count articles) feeds=$(count feeds) folders=$(count folders) tags=$(count tags)"
echo "==> Backup: $BAK"
echo "==> Done."
