# Search synonym expansion (CN–EN)

Search-time expansion of bilingual (and other) aliases so strict AND search
does not require every surface form in the same article.

This document is the design companion to the query-language contract in
[`search.md`](search.md). Engine mapping and operators stay in `search.md`;
**when and how terms expand** lives here.

## Why

UI list search is **strict**: bare terms combine with **AND**. A query like
`Trump 特朗普` therefore demands both strings in one article and often returns
**zero rows**, even though the user meant a single person.

Wordcloud entities already list bilingual aliases (e.g. `person.trump` →
`Trump`, `特朗普`, …). FTS historically ignored that gazetteer. Case folding
(`AI` / `ai`) is handled by FTS5 and is **not** the gap this feature fills.

## Source of truth

| Source | Role for FTS synonyms |
| --- | --- |
| `wordcloud-entities.json` / `WordCloudDict` | **Yes** — alias sets for expansion |
| `tag_aliases` (interest-tag auto-tag) | **No** — never consulted by FTS |

Aliases are the same strings used for wordcloud entity matching (normalized
lowercase trim). Search uses **exact alias lookup** of whole bare query tokens,
not the longest-span matcher used when indexing article text.

## Semantics

1. Parse the user query into the existing AST ([`search.md`](search.md)).
2. For each **bare** term (optional `title:` / `body:` field), look up a synonym
   group in the wordcloud dict.
3. On hit, replace the term with an **OR** of all aliases in that entity
   (canonical + aliases), each compiled as a **whole token** (quoted, **no**
   automatic trailing `*`). Multi-word aliases stay exact phrases. This keeps
   short entity forms like `AI`/`ai` from matching `against`/`aid`, and matches
   word-cloud click semantics (entity → whole word, not typed free-prefix).
4. On miss, compile the term unchanged.
5. **Across** synonym groups (and unmatched terms), keep the mode’s join:
   - **strict** (UI): **AND**
   - **recall** (RAG / CLI default): **OR**
6. If several query tokens resolve to the **same** entity, collapse to **one**
   OR-group (do not AND the same group with itself).

### Illustrative MATCH (strict)

| User | Compiled idea |
| --- | --- |
| `特朗普` | `("Trump" OR "特朗普" OR …)` (whole tokens) |
| `Trump 特朗普` | Same single OR-group (both map to `person.trump`) |
| `Trump china` | `(Trump-group) AND (china-group)` — both whole-token when entities |
| `ai` (entity) | `("AI" OR "ai" OR …)` — never `"ai"*` |
| `"Trump 特朗普"` | Phrase — **no** expansion |
| `feed:Reuters Trump` | Feed SQL unchanged; `Trump` may expand in FTS |

## UI: chip merge

List-header search chips are **one chip per synonym group** (or per unmatched
token), not one chip per whitespace token.

- Label prefers the user-typed surface (or canonical if the UI has entity data).
- Clearing a chip removes every query token that belonged to that group.
- Snippet/title highlighting uses the **union** of aliases for groups in the
  query so an English query can still highlight Chinese hits (and vice versa).

## Phases

| Phase | Scope |
| --- | --- |
| **P0** | Search-time expansion + chip merge + highlight needles; **no** DB migration |
| **P1** | ✅ Copy-on-write wordcloud entity editing + **Add entity** (Settings → Word cloud) — rename existing rows or promote residual cloud tokens; reload/backfill pick up new display names |

## Non-goals

- Rewriting FTS-indexed article text or adding entity-id columns.
- Fuzzy / embedding / spell-correct search.
- Using `tag_aliases` for retrieval.
- Expanding inside quoted phrases or `feed:` values.
- Shipping a full entity editor in P0.

## Migration

**None for P0.** No schema change. Synonyms come from the entity file loaded into
`WordCloudDict` (shared seed or papr COW overlay after Settings edits).

## Acceptance examples

| # | Input (strict) | Expect |
| --- | --- | --- |
| 1 | `特朗普` | Hits articles that only say “Trump” |
| 2 | `Trump` | Hits articles that only say “特朗普” |
| 3 | `Trump 特朗普` | Not empty solely because both forms were ANDed |
| 4 | `Trump china` | Still requires both *groups* (AND across entities) |
| 5 | `"Trump 特朗普"` | Phrase match only; no synonym OR |
| 6 | Chip merge | Two tokens → one chip when same entity; clear drops both |
| 7 | `tag_aliases` only | Does not change FTS results |

## Implementation anchors

- Compile: `crates/papr-core/src/search.rs` (`compile_search`)
- Dict: `crates/papr-core/src/wordcloud_dict.rs`
- List chips: `src/components/ArticleList.tsx`
- Highlight: `src/lib/searchHighlight.ts`

## Related docs

- [搜索、词云与标签（用户指南）](user-search-and-tags.md) — practical UI guide (Chinese)
- [Search query language](search.md) — operators, modes, FTS mapping
- [CLI reference](cli.md) — `papr search` / `--and`
