import type { WordCloudEntity } from "../types";

/** One list-header chip: a synonym group or a lone unmatched token. */
export type SearchChip = {
  /** Display label (prefer first user-typed surface). */
  label: string;
  /** Whitespace tokens removed when the chip is cleared. */
  tokens: string[];
  /** Wordcloud entity id when tokens collapse to one synonym group. */
  entityId?: string;
};

type AliasIndex = Map<string, { id: string; canonical: string }>;

function normalizeAlias(raw: string): string {
  return raw.trim().toLowerCase();
}

/** Build normalized alias → entity for exact lookup (same rules as WordCloudDict). */
export function buildAliasIndex(entities: WordCloudEntity[]): AliasIndex {
  const map: AliasIndex = new Map();
  // Deterministic: smaller entity id wins on conflicts.
  const sorted = [...entities].sort((a, b) => a.id.localeCompare(b.id));
  for (const ent of sorted) {
    const id = ent.id.trim();
    const canonical = ent.canonical.trim();
    if (!id || !canonical) continue;
    const surfaces = [canonical, ...(ent.aliases ?? [])];
    for (const s of surfaces) {
      const key = normalizeAlias(s);
      if (!key || map.has(key)) continue;
      map.set(key, { id, canonical });
    }
  }
  return map;
}

function lookup(
  index: AliasIndex,
  token: string,
): { id: string; canonical: string } | undefined {
  // Strip leading unary minus / trailing * for entity lookup (chip surface keeps raw).
  const cleaned = token.replace(/^\-/, "").replace(/\*$/, "");
  if (!cleaned) return undefined;
  // Skip operators and field filters for grouping.
  const upper = cleaned.toUpperCase();
  if (upper === "OR" || upper === "AND" || upper === "NOT") return undefined;
  if (/^(title|body|feed):/i.test(cleaned)) return undefined;
  if (cleaned.startsWith('"') || cleaned === "(" || cleaned === ")") return undefined;
  return index.get(normalizeAlias(cleaned));
}

/**
 * Split a list-search string into chips, merging consecutive/same-entity tokens
 * that share a wordcloud synonym group into one chip.
 */
export function searchChips(
  query: string,
  entities: WordCloudEntity[] = [],
): SearchChip[] {
  const parts = query.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return [];
  const index = buildAliasIndex(entities);
  const chips: SearchChip[] = [];

  for (const token of parts) {
    const hit = lookup(index, token);
    if (hit) {
      const prev = chips[chips.length - 1];
      if (prev?.entityId === hit.id) {
        prev.tokens.push(token);
        continue;
      }
      // Also merge non-adjacent same-entity tokens into the existing chip
      // so `特朗普 foo Trump` still shows one Trump chip + foo (order: first seen).
      const existing = chips.find((c) => c.entityId === hit.id);
      if (existing) {
        existing.tokens.push(token);
        continue;
      }
      chips.push({
        label: token.replace(/^\-/, "").replace(/\*$/, "") || hit.canonical,
        tokens: [token],
        entityId: hit.id,
      });
      continue;
    }
    chips.push({ label: token, tokens: [token] });
  }
  return chips;
}

/** Remove every token belonging to `chip` from the query string. */
export function removeSearchChip(query: string, chip: SearchChip): string | null {
  const remove = new Set(chip.tokens);
  const next = query
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .filter((p) => !remove.has(p))
    .join(" ");
  return next || null;
}

/**
 * Additive word-cloud click: toggle synonym group if `term` maps to an entity
 * already represented; otherwise append (or remove exact token match).
 */
export function mergeAdditiveSearchTerm(
  current: string,
  term: string,
  entities: WordCloudEntity[] = [],
): string | null {
  const t = term.trim();
  if (!t) return current.trim() || null;
  const cur = current.trim();
  if (!cur) return t;

  const index = buildAliasIndex(entities);
  const hit = lookup(index, t);
  const parts = cur.split(/\s+/).filter(Boolean);

  if (hit) {
    const groupTokens = parts.filter((p) => lookup(index, p)?.id === hit.id);
    if (groupTokens.length > 0) {
      const drop = new Set(groupTokens);
      const next = parts.filter((p) => !drop.has(p)).join(" ");
      return next || null;
    }
    return `${cur} ${t}`;
  }

  if (parts.includes(t)) {
    return parts.filter((p) => p !== t).join(" ") || null;
  }
  return `${cur} ${t}`;
}

/** Expand highlight needles with synonym aliases for groups present in the query. */
export function expandHighlightTerms(
  baseTerms: string[],
  entities: WordCloudEntity[],
): string[] {
  if (entities.length === 0) return baseTerms;
  const index = buildAliasIndex(entities);
  const byId = new Map(entities.map((e) => [e.id, e]));
  const out: string[] = [];
  const seen = new Set<string>();
  const push = (s: string) => {
    const key = s.toLowerCase();
    if (!s || seen.has(key)) return;
    seen.add(key);
    out.push(s);
  };
  for (const term of baseTerms) {
    const hit = index.get(normalizeAlias(term));
    if (hit) {
      const ent = byId.get(hit.id);
      if (ent) {
        push(ent.canonical);
        for (const a of ent.aliases ?? []) push(a);
        continue;
      }
    }
    push(term);
  }
  return out;
}
