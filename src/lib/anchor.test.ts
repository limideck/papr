// Unit tests for the highlight re-anchoring algorithm (feature F7).
// `anchor.ts` is pure and DOM-free, so it is fully exercisable in node.

import { describe, it, expect } from "vitest";
import { findAnchor, captureContext, CONTEXT_LEN, type HighlightAnchor } from "./anchor";

const TEXT =
  "The quick brown fox jumps over the lazy dog. " +
  "A second sentence mentions the quick brown fox again here.";

describe("findAnchor — exact offset", () => {
  it("resolves a highlight at its stored offset", () => {
    const quote = "quick brown fox";
    const offset = TEXT.indexOf(quote);
    const a: HighlightAnchor = { quote, prefix: "The ", suffix: " jumps", textOffset: offset };
    expect(findAnchor(TEXT, a)).toEqual({ start: offset, end: offset + quote.length });
  });

  it("resolves a highlight at the very start of the text", () => {
    const a: HighlightAnchor = { quote: "The quick", prefix: "", suffix: " brown", textOffset: 0 };
    expect(findAnchor(TEXT, a)).toEqual({ start: 0, end: 9 });
  });
});

describe("findAnchor — shifted offset", () => {
  it("finds the quote when a prefix shifted the offset forward", () => {
    const quote = "lazy dog";
    const realOffset = TEXT.indexOf(quote);
    // The stored offset is stale (text had a heading prepended since).
    const a: HighlightAnchor = {
      quote,
      prefix: "over the ",
      suffix: ".",
      textOffset: realOffset - 50,
    };
    expect(findAnchor(TEXT, a)).toEqual({ start: realOffset, end: realOffset + quote.length });
  });

  it("finds a unique quote even with a wildly wrong offset", () => {
    const quote = "second sentence";
    const realOffset = TEXT.indexOf(quote);
    const a: HighlightAnchor = { quote, prefix: "A ", suffix: " mentions", textOffset: 99999 };
    expect(findAnchor(TEXT, a)).toEqual({ start: realOffset, end: realOffset + quote.length });
  });
});

describe("findAnchor — duplicate quotes disambiguated by context", () => {
  const quote = "quick brown fox";
  const first = TEXT.indexOf(quote);
  const second = TEXT.indexOf(quote, first + 1);

  it("picks the first occurrence by its prefix context", () => {
    const a: HighlightAnchor = {
      quote,
      prefix: "The ",
      suffix: " jumps over",
      textOffset: 9999, // offset unhelpful — context must decide
    };
    expect(findAnchor(TEXT, a)).toEqual({ start: first, end: first + quote.length });
  });

  it("picks the second occurrence by its suffix context", () => {
    const a: HighlightAnchor = {
      quote,
      prefix: "mentions the ",
      suffix: " again here",
      textOffset: 9999,
    };
    expect(findAnchor(TEXT, a)).toEqual({ start: second, end: second + quote.length });
  });

  it("breaks a context tie by proximity to the stored offset", () => {
    // Both occurrences have no matching context — fall back to the offset.
    const a: HighlightAnchor = { quote, prefix: "zzz", suffix: "zzz", textOffset: second };
    expect(findAnchor(TEXT, a)).toEqual({ start: second, end: second + quote.length });
  });
});

describe("findAnchor — quote not found", () => {
  it("returns null when the quote is absent", () => {
    const a: HighlightAnchor = {
      quote: "nonexistent phrase",
      prefix: "",
      suffix: "",
      textOffset: 0,
    };
    expect(findAnchor(TEXT, a)).toBeNull();
  });

  it("returns null for an empty quote", () => {
    const a: HighlightAnchor = { quote: "", prefix: "a", suffix: "b", textOffset: 0 };
    expect(findAnchor(TEXT, a)).toBeNull();
  });

  it("returns null when the quote is longer than the whole text", () => {
    const a: HighlightAnchor = { quote: TEXT + " extra", prefix: "", suffix: "", textOffset: 0 };
    expect(findAnchor(TEXT, a)).toBeNull();
  });
});

describe("findAnchor — whitespace and edge cases", () => {
  it("anchors a whitespace-only quote", () => {
    const text = "a   b";
    const a: HighlightAnchor = { quote: "   ", prefix: "a", suffix: "b", textOffset: 1 };
    expect(findAnchor(text, a)).toEqual({ start: 1, end: 4 });
  });

  it("handles an empty text", () => {
    const a: HighlightAnchor = { quote: "x", prefix: "", suffix: "", textOffset: 0 };
    expect(findAnchor("", a)).toBeNull();
  });

  it("anchors a quote at the exact end of the text", () => {
    const text = "hello world";
    const a: HighlightAnchor = { quote: "world", prefix: "hello ", suffix: "", textOffset: 6 };
    expect(findAnchor(text, a)).toEqual({ start: 6, end: 11 });
  });

  it("rejects a negative stored offset but still finds the quote", () => {
    const a: HighlightAnchor = {
      quote: "lazy dog",
      prefix: "the ",
      suffix: ".",
      textOffset: -10,
    };
    const real = TEXT.indexOf("lazy dog");
    expect(findAnchor(TEXT, a)).toEqual({ start: real, end: real + 8 });
  });
});

describe("captureContext", () => {
  it("captures up to CONTEXT_LEN characters on each side", () => {
    const text = "a".repeat(100) + "QUOTE" + "b".repeat(100);
    const start = 100;
    const end = 105;
    const { prefix, suffix } = captureContext(text, start, end);
    expect(prefix).toBe("a".repeat(CONTEXT_LEN));
    expect(suffix).toBe("b".repeat(CONTEXT_LEN));
  });

  it("clamps the prefix window at the start of the text", () => {
    const { prefix, suffix } = captureContext("hi there", 0, 2);
    expect(prefix).toBe("");
    expect(suffix).toBe(" there");
  });

  it("round-trips: a captured context re-anchors the same range", () => {
    const quote = "brown fox";
    const start = TEXT.indexOf(quote);
    const end = start + quote.length;
    const ctx = captureContext(TEXT, start, end);
    const a: HighlightAnchor = { quote, ...ctx, textOffset: start };
    expect(findAnchor(TEXT, a)).toEqual({ start, end });
  });
});
