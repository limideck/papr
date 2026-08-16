# Papr

Local-first RSS reader (desktop + web) with a shared SQLite core and an
agent-facing CLI.

## Docs

- [搜索、词云与标签（用户指南）](docs/user-search-and-tags.md) — how to search, use the word cloud, and manage interest / AI tags
- [Search query language](docs/search.md) — boolean FTS syntax (AND / OR / NOT / phrases / fields)
- [Search synonym expansion](docs/search-synonyms.md) — CN–EN / wordcloud entity aliases at search time
- [CLI](docs/cli.md) — `papr` for agents and the shell
- Skill: [`skills/papr-rss`](skills/papr-rss/SKILL.md)

## Word-cloud entity editing

Admins can change entity **display canonicals** (e.g. `ai` → `AI`) in
**Settings → Word cloud → Entity dictionary**. Matching stays case-insensitive;
the previous form is kept as an alias.

**Important:** lowercase `ai` in the cloud is often a **residual** token (not in
the gazetteer). Searching Entity dictionary for `ai` finds nothing in that case —
use **Add entity** instead of Edit:

1. Settings → Word cloud → Entity dictionary → **Add entity**
2. Canonical `AI`, group as needed (aliases optional; lowercase `ai` is kept
   automatically when casing differs)
3. Save (first edit copy-on-writes the seed into the papr COW path)
4. Term index → **Backfill now** until remaining is 0

After backfill, the cloud shows `AI` and former `ai` counts merge into it.

Existing gazetteer rows: open **Edit** on the row, change Canonical, Save, then
backfill the same way.

**Load priority** for `wordcloud-entities.json`:

1. `PAPR_WORDCLOUD_DIR` (when set to a non-seed directory)
2. Local copy-on-write file if it exists
3. Shared seed `/product/osinttools/data/dashboard`

**COW path** (first edit copies the seed here; the shared file is never written):

- `PAPR_WORDCLOUD_COW_DIR` if set, else
- `{dirname(PAPR_DB)}/wordcloud/wordcloud-entities.json` (e.g. `/product/papr/wordcloud/…`)

After saving a canonical, run **Term index → Backfill** (or wait for the
background worker) so the cloud shows the new spelling.

Optional env: `PAPR_WORDCLOUD_DIR` (override seed/read-write dir),
`PAPR_WORDCLOUD_COW_DIR` (override overlay dir).

## Ops (deploy)

Schema changes are **append-only migrations** in `papr-core` (`db.rs`). On open,
the writer connection runs `MIGRATIONS.to_latest` — production merge is
restart/redeploy so the new binary opens the live SQLite file and applies any
new versions (e.g. v30 `tag_aliases`). Never drop/rebuild `tags` or
`article_tags` for additive features.

在本机（已配好 scripts/deploy.env）：

```sh
scripts/restart.sh
# 或
scripts/deploy.sh --restart-only
```

或在服务器上：

```sh
systemctl restart papr-server
systemctl status papr-server
curl -s http://127.0.0.1:7400/api/health
```

```sh
git tag v0.0.1
git push origin v0.0.1
```

```sh
scripts/deploy.sh --from-release latest
```
