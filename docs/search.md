# Papr search query language

Keyword full-text search over article title and body, powered by **SQLite FTS5**.
This document is the **semantic contract** for the UI list search, CLI, RAG
retrieval, and unit tests. Implementations must not invent operators that are
not listed here.

Engine: SQLite FTS5 only — not Elasticsearch / Meilisearch / Typesense.

## Overview

Papr search is a small boolean query language. The **default mode for the UI
article list** is **strict** (precise boolean): adding words narrows results.

| Mode | Who uses it | Implicit join between bare terms |
| --- | --- | --- |
| **strict** | Desktop / web article list search | **AND** (every term must match) |
| **recall** | RAG context retrieval; CLI `papr search` by default | **OR** (any term may match) |

Operators (`OR`, `NOT`, `-`, quotes, parentheses, field filters) work the same
in both modes. Only the default join for adjacent bare terms differs.

CLI: `papr search` defaults to **recall**; pass `--and` for **strict** (same
semantics as the UI).

## Lexicon

- **Whitespace** separates terms; consecutive whitespace is insignificant.
- **Quoted phrases** use ASCII double quotes: `"interest rate"`.
- **Operators** are case-insensitive: `OR`, `AND`, `NOT`.
- **Unary minus** `-` immediately before a term/group means NOT.
- **Prefix** `*` may be attached to an unquoted term (`chin*`). Unquoted terms
  also get an automatic trailing `*` (prefix match) unless already present,
  **except** short Latin tokens (ASCII alphanumeric, length ≤ 3) such as `AI` /
  `ai` / `US`, which compile as whole-token matches (no auto-`*`) so they do
  not hit longer words like `against` / `aid`. Explicit `ai*` still forces prefix.
- **Field filters** use `name:value` with no space around `:`.
- Letters and digits form terms; other punctuation inside an *unquoted* token
  splits it into separate terms (aligned with FTS5 `unicode61`, which indexes
  `rust-lang` as `rust` + `lang`).
- Empty / all-punctuation / unparseable queries yield **no rows** (never pass
  a raw user string to `MATCH`).

## Operators

| Syntax | Meaning | Example | Expected |
| --- | --- | --- | --- |
| `a b` | Implicit AND (strict) | `Trump china` | Same article contains both |
| `"a b"` | Phrase (adjacent tokens) | `"Trump China"` | Contiguous phrase |
| `a OR b` | Disjunction | `Trump OR Biden` | At least one term |
| `a AND b` | Explicit AND (same as space) | `Trump AND china` | Same as `Trump china` |
| `a -b` / `a NOT b` | Exclusion | `Trump -tariff` | Has Trump, no tariff |
| `(a OR b) c` | Grouping | `(Trump OR Biden) china` | Group, then AND |
| `a*` / default prefix | Prefix | `chin*` / `china` → `china*` | Prefix expansion (not for short Latin ≤3) |
| `title:a` | Title column only | `title:Trump china` | Trump in title; china any field |
| `body:a` | Body column only | `body:sanctions` | Term in body |
| `feed:name` | Feed title filter (SQL) | `feed:Reuters Trump` | Feed title prefix + FTS |
| Compound | Mix freely | `title:Trump (china OR taiwan) -opinion` | All conditions |

### Sample queries

```text
# AND (default in strict / UI)
Trump china

# Phrase
"interest rate"

# OR + AND
(Trump OR Biden) china

# Exclusion
Trump -china
Trump NOT china

# Fields
title:Trump body:tariff
feed:Nikkei economy

# Compound
title:"Federal Reserve" (inflation OR rates) -opinion
```

## Field filters

| Filter | Scope | Notes |
| --- | --- | --- |
| `title:` | FTS `title` column | Prefix on unquoted values; phrases allowed |
| `body:` | FTS `body` column | Same |
| `feed:` | Feed title (`feeds.title`) | Case-insensitive prefix match; **not** part of FTS MATCH. Multiple `feed:` filters AND together. |

Unknown field names are treated as a normal term that includes the colon text
only if tokenized; prefer the listed filters.

## Sort

| Context | Default order |
| --- | --- |
| No search (browse) | Chronological (`published_at` / `fetched_at`), newest or oldest per UI toggle |
| Search active | **Relevance** (FTS5 `bm25` / `rank`), then date as secondary |
| Search + user chooses date | Chronological only (same toggle as browse) |

## Modes (strict vs recall)

**strict** — UI list search. `Trump china` requires both terms. Use when the
user is narrowing a feed or library.

**recall** — RAG (`search_articles_for_rag`) and CLI `papr search` (default).
`what does privacy regulation say about borrow checker` should still return
articles that match *some* keywords. Pass CLI `--and` for strict.

RAG is **not** changed to force AND; that would empty results on natural-language
questions.

## Engine mapping (user string → FTS5 MATCH)

User input is parsed into an AST, then compiled to a **safe** FTS5 expression:

| User | Mode | Compiled MATCH (illustrative) |
| --- | --- | --- |
| `Trump china` | strict | `"Trump"* AND "china"*` |
| `Trump china` | recall | `("Trump"* OR "china"*)` |
| `"Trump China"` | either | `"Trump China"` |
| `Trump OR Biden` | either | `("Trump"* OR "Biden"*)` |
| `Trump -tariff` | either | `"Trump"* NOT "tariff"*` |
| `(Trump OR Biden) china` | strict | `("Trump"* OR "Biden"*) AND "china"*` |
| `title:Trump` | either | `title:"Trump"*` |
| `chin*` | either | `"chin"*` |
| `AI` / `ai` | either | `"AI"` / `"ai"` (whole token; no auto-`*`) |
| `feed:Reuters Trump` | strict | MATCH `"Trump"*` **and** SQL `feed title LIKE 'Reuters%'` (ci) |
| `   ` / `???` | either | no rows (empty / match-nothing) |

Rules:

1. Every bare term is double-quoted; internal `"` doubled per FTS5.
2. Never concatenate raw user text into `MATCH`.
3. `feed:` filters become SQL predicates on `feeds.title`, not FTS tokens.
4. Default prefix `*` applies to unquoted terms only; quoted phrases are exact.
   Short Latin (≤3 ASCII alnum) skips auto-prefix. Wordcloud / synonym-expanded
   terms always compile as whole tokens (see [search-synonyms.md](search-synonyms.md)).
5. After parse, bare terms may be **synonym-expanded** (see below) before the
   final MATCH string is built.

## Synonym expansion

Bare terms may expand via **wordcloud entity aliases** (e.g. `Trump` /
`特朗普` → one OR-group). Within a synonym group forms are **OR**’d; across
groups, strict mode still **AND**s. Quoted phrases and `feed:` values do not
expand. Interest-tag `tag_aliases` are **not** used for FTS.

Full design, chip-merge UX, phases, and acceptance cases:
**[search-synonyms.md](search-synonyms.md)**.

## Limits and non-goals

**Limits (v1)**

- CJK: no dedicated trigram tokenizer; CJK queries depend on `unicode61` token
  boundaries and may be weaker than Latin. Synonym expansion mitigates known
  bilingual entity pairs; it is not a general CJK tokenizer.
- No spell correction / fuzzy edit distance.
- No regular expressions.
- No Elasticsearch-style DSL or nested JSON queries.

**Non-goals / future** (not in this release)

- Embedding / semantic search (Phase 5)
- Changing RAG default from recall to strict
- External search engines
- Using `tag_aliases` as an FTS synonym source (auto-tag only)

## Acceptance examples

These cases are the unit-test checklist. Expected compiled forms are for
**strict** unless noted.

| # | Input | Expect |
| --- | --- | --- |
| 1 | `Trump china` | AND of two prefix terms; hits must contain both |
| 2 | `"Trump China"` | Phrase MATCH; adjacent tokens |
| 3 | `Trump OR Biden` | OR of two prefix terms |
| 4 | `Trump -china` | Trump present, china absent |
| 5 | `Trump NOT china` | Same as #4 |
| 6 | `(Trump OR Biden) china` | Grouped OR, then AND china |
| 7 | `title:Trump` | Restricted to title column |
| 8 | `body:sanctions` | Restricted to body column |
| 9 | `feed:Reuters` | Feed-title prefix filter (may combine with other terms) |
| 10 | `` (empty) / `???` | Zero rows; no FTS error |
| 11 | `Trump china` (recall) | OR join; may match either term |
| 12 | Search active | Default sort by relevance (`rank`), date secondary |
| 13 | `Trump 特朗普` (strict, after synonym expansion) | Same entity → one OR-group; see [search-synonyms.md](search-synonyms.md) |
| 14 | `AI` / `ai` | Whole token `"AI"` / `"ai"` (no auto-`*`); does not match `against` |

## Related docs

- [搜索、词云与标签（用户指南）](user-search-and-tags.md) — practical UI guide (Chinese)
- [Search synonym expansion](search-synonyms.md) — CN–EN / entity aliases at search time
- [CLI reference](cli.md) — `papr search` and `--and`
- Skill: [`skills/papr-rss/SKILL.md`](../skills/papr-rss/SKILL.md)
