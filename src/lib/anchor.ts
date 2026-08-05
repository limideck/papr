// Highlight re-anchoring (feature F7).
//
// A stored highlight records the quoted text, a short context window before
// and after it (`prefix` / `suffix`), and the character offset of the quote in
// the article's rendered plain text. When the article is reopened the plain
// text may have shifted — full-text extraction can replace a short feed
// snippet with the whole page — so the offset alone is not reliable.
//
// `findAnchor` is a pure function (no DOM): given the current plain text and a
// highlight descriptor it returns the best character range, trying, in order:
//   1. the exact stored offset (fast path, the common case),
//   2. the occurrence whose surrounding context best matches prefix/suffix,
//   3. the first occurrence of the quote anywhere.
// It returns `null` when the quote cannot be found at all.

/** The anchoring inputs persisted with every highlight. */
export interface HighlightAnchor {
  quote: string;
  prefix: string;
  suffix: string;
  textOffset: number;
}

/** A resolved character range `[start, end)` within the plain text. */
export interface AnchorRange {
  start: number;
  end: number;
}

/** How many context characters to capture on each side when a highlight is
 *  first created. Exported so the Reader uses the same window the search below
 *  assumes. */
export const CONTEXT_LEN = 32;

/** Collect every index at which `needle` occurs in `haystack`. */
function allOccurrences(haystack: string, needle: string): number[] {
  const hits: number[] = [];
  if (needle.length === 0) return hits;
  let from = 0;
  for (;;) {
    const i = haystack.indexOf(needle, from);
    if (i === -1) break;
    hits.push(i);
    from = i + 1; // overlapping occurrences are still distinct anchors
  }
  return hits;
}

/** Number of trailing characters shared by `a` and `b`. */
function commonSuffixLen(a: string, b: string): number {
  let n = 0;
  while (n < a.length && n < b.length && a[a.length - 1 - n] === b[b.length - 1 - n]) {
    n++;
  }
  return n;
}

/** Number of leading characters shared by `a` and `b`. */
function commonPrefixLen(a: string, b: string): number {
  let n = 0;
  while (n < a.length && n < b.length && a[n] === b[n]) n++;
  return n;
}

/**
 * Score how well the text around `pos` matches the stored context. Higher is
 * better; the maximum is `prefix.length + suffix.length`.
 */
function contextScore(
  text: string,
  pos: number,
  quoteLen: number,
  anchor: HighlightAnchor,
): number {
  const before = text.slice(Math.max(0, pos - anchor.prefix.length), pos);
  const after = text.slice(pos + quoteLen, pos + quoteLen + anchor.suffix.length);
  return commonSuffixLen(before, anchor.prefix) + commonPrefixLen(after, anchor.suffix);
}

/**
 * Resolve a highlight to a character range within `text`, or `null` if its
 * quote is absent. Pure and DOM-free, so it is exhaustively unit-testable.
 */
export function findAnchor(text: string, anchor: HighlightAnchor): AnchorRange | null {
  const quote = anchor.quote;
  if (quote.length === 0) return null;

  // 1. Exact offset — the text has not shifted since the highlight was made.
  const off = anchor.textOffset;
  if (off >= 0 && off + quote.length <= text.length) {
    if (text.slice(off, off + quote.length) === quote) {
      return { start: off, end: off + quote.length };
    }
  }

  // 2. Context search — pick the occurrence with the best prefix/suffix match.
  const hits = allOccurrences(text, quote);
  if (hits.length === 0) return null;
  if (hits.length === 1) {
    return { start: hits[0], end: hits[0] + quote.length };
  }

  let best = hits[0];
  let bestScore = -1;
  let bestDistance = Infinity;
  for (const pos of hits) {
    const score = contextScore(text, pos, quote.length, anchor);
    const distance = Math.abs(pos - off);
    // Prefer the strongest context match; break ties by proximity to the
    // stored offset, so a duplicate quote still lands near where it was.
    if (score > bestScore || (score === bestScore && distance < bestDistance)) {
      best = pos;
      bestScore = score;
      bestDistance = distance;
    }
  }
  // 3. Fallback is implicit: if no context matched, `best` is still a real
  //    occurrence (the first one, given the tie-breaking above).
  return { start: best, end: best + quote.length };
}

/**
 * Capture the surrounding-context window for a new highlight. `text` is the
 * full plain text, `start`/`end` the selected range. Returns the prefix and
 * suffix windows used later by `findAnchor`.
 */
export function captureContext(
  text: string,
  start: number,
  end: number,
): { prefix: string; suffix: string } {
  return {
    prefix: text.slice(Math.max(0, start - CONTEXT_LEN), start),
    suffix: text.slice(end, end + CONTEXT_LEN),
  };
}
