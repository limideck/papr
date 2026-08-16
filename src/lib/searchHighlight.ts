import type { WordCloudEntity } from "../types";
import { expandHighlightTerms } from "./searchChips";

/** Split like Rust `is_alphanumeric` (letters + numbers, including CJK). */
const NON_ALNUM = /[^\p{L}\p{N}]+/u;

/** Han / kana / hangul — these needles stay substring matches. */
const CJK_GLYPH = /[\u3040-\u30ff\u3400-\u9fff\uac00-\ud7af\uf900-\ufaff]/u;

/** Extract highlight needles from a search query (mirrors docs/search.md terms). */
export function searchHighlightTerms(
  query: string,
  entities: WordCloudEntity[] = [],
): string[] {
  const terms: string[] = [];
  const re =
    /(?:"([^"]*)")|(?:(?:title|body|feed):(?:"([^"]*)"|(\S+)))|(\S+)/gi;
  let m: RegExpExecArray | null;
  while ((m = re.exec(query)) !== null) {
    const phrase = m[1];
    const fieldPhrase = m[2];
    const fieldTerm = m[3];
    const bare = m[4];
    if (phrase != null) {
      if (phrase.trim()) terms.push(phrase.trim());
      continue;
    }
    const fieldRaw = (fieldPhrase ?? fieldTerm ?? "").trim();
    if (fieldRaw) {
      // Skip feed: values for text highlighting inside title/snippet.
      const full = m[0] ?? "";
      if (/^feed:/i.test(full)) continue;
      for (const part of fieldRaw.split(NON_ALNUM)) {
        if (part) terms.push(part.replace(/\*$/, ""));
      }
      continue;
    }
    if (bare) {
      const upper = bare.toUpperCase();
      if (upper === "OR" || upper === "AND" || upper === "NOT") continue;
      if (bare === "(" || bare === ")") continue;
      const cleaned = bare.replace(/^\-/, "").replace(/\*$/, "");
      for (const part of cleaned.split(NON_ALNUM)) {
        if (part) terms.push(part);
      }
    }
  }
  const base = [...new Set(terms.filter(Boolean))];
  return expandHighlightTerms(base, entities);
}

/** Latin (and digit) needles use Unicode letter/number boundaries — not `\b`
 *  alone, which misses some edges — so `ai` does not light up inside Against. */
function usesWordBoundaries(term: string): boolean {
  if (!term || CJK_GLYPH.test(term)) return false;
  return /[\p{Script=Latin}\p{N}]/u.test(term);
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function needlePattern(term: string): string {
  const escaped = escapeRegExp(term).replace(/\s+/g, "\\s+");
  if (usesWordBoundaries(term)) {
    return `(?<![\\p{L}\\p{N}])${escaped}(?![\\p{L}\\p{N}])`;
  }
  return escaped;
}

function normHit(s: string): string {
  return s.toLowerCase().replace(/\s+/g, " ");
}

/** Split `text` into plain / mark segments for query-term highlighting. */
export function highlightSegments(
  text: string,
  terms: string[],
): Array<{ text: string; hit: boolean }> {
  if (!text || terms.length === 0) return [{ text, hit: false }];
  // Longer needles first so multi-word aliases win over substrings.
  const sorted = [...terms].sort((a, b) => b.length - a.length);
  const patterns = sorted.map(needlePattern).filter(Boolean);
  if (patterns.length === 0) return [{ text, hit: false }];
  const re = new RegExp(`(${patterns.join("|")})`, "giu");
  const parts = text.split(re);
  const needles = new Set(terms.map(normHit));
  return parts.filter(Boolean).map((p) => ({
    text: p,
    hit: needles.has(normHit(p)),
  }));
}
