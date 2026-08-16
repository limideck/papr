import { describe, expect, it } from "vitest";
import { highlightSegments, searchHighlightTerms } from "./searchHighlight";
import type { WordCloudEntity } from "../types";

const entities: WordCloudEntity[] = [
  {
    id: "person.trump",
    canonical: "Trump",
    group: "person",
    aliases: ["trump", "特朗普", "川普"],
  },
];

describe("searchHighlightTerms", () => {
  it("extracts AND terms and skips operators", () => {
    expect(searchHighlightTerms("Trump OR Biden -opinion")).toEqual([
      "Trump",
      "Biden",
      "opinion",
    ]);
  });

  it("keeps phrases", () => {
    expect(searchHighlightTerms('"Federal Reserve" rates')).toEqual([
      "Federal Reserve",
      "rates",
    ]);
  });

  it("skips feed filters", () => {
    expect(searchHighlightTerms("feed:Reuters Trump")).toEqual(["Trump"]);
  });

  it("keeps CJK tokens", () => {
    expect(searchHighlightTerms("特朗普")).toEqual(["特朗普"]);
  });

  it("expands synonym aliases when entities provided", () => {
    const terms = searchHighlightTerms("Trump", entities);
    expect(terms).toEqual(
      expect.arrayContaining(["Trump", "特朗普", "川普"]),
    );
  });

  it("casefold ai still extracted", () => {
    expect(searchHighlightTerms("AI")).toEqual(["AI"]);
    expect(searchHighlightTerms("ai")).toEqual(["ai"]);
  });
});

describe("highlightSegments", () => {
  it("marks matching spans", () => {
    const segs = highlightSegments("Trump visits China", ["Trump", "China"]);
    expect(segs.filter((s) => s.hit).map((s) => s.text)).toEqual([
      "Trump",
      "China",
    ]);
  });

  it("highlights CN alias for EN query needles", () => {
    const needles = searchHighlightTerms("Trump", entities);
    const segs = highlightSegments("特朗普访问中国", needles);
    expect(segs.some((s) => s.hit && s.text === "特朗普")).toBe(true);
  });

  it("Latin ai uses word boundaries (not inside Taiwan/Against/Restraint)", () => {
    const needles = ["ai", "AI"];
    for (const word of ["Taiwan", "Against", "Restraint"]) {
      const segs = highlightSegments(word, needles);
      expect(segs.some((s) => s.hit)).toBe(false);
    }
    expect(
      highlightSegments("AI leads", needles)
        .filter((s) => s.hit)
        .map((s) => s.text),
    ).toEqual(["AI"]);
    expect(
      highlightSegments("about ai regulation", needles)
        .filter((s) => s.hit)
        .map((s) => s.text),
    ).toEqual(["ai"]);
  });

  it("CJK needles stay substring matches", () => {
    const segs = highlightSegments("特朗普访问中国", ["特朗普"]);
    expect(segs.some((s) => s.hit && s.text === "特朗普")).toBe(true);
  });
});
