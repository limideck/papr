import { describe, expect, it } from "vitest";
import {
  mergeAdditiveSearchTerm,
  removeSearchChip,
  searchChips,
} from "./searchChips";
import type { WordCloudEntity } from "../types";

const entities: WordCloudEntity[] = [
  {
    id: "person.trump",
    canonical: "Trump",
    group: "person",
    aliases: ["trump", "特朗普", "川普"],
  },
  {
    id: "country.china",
    canonical: "China",
    group: "country",
    aliases: ["china", "中国"],
  },
  {
    id: "person.biden",
    canonical: "Biden",
    group: "person",
    aliases: ["biden", "拜登"],
  },
];

describe("searchChips", () => {
  it("merges same-entity tokens into one chip", () => {
    const chips = searchChips("Trump 特朗普", entities);
    expect(chips).toHaveLength(1);
    expect(chips[0].entityId).toBe("person.trump");
    expect(chips[0].tokens).toEqual(["Trump", "特朗普"]);
    expect(chips[0].label).toBe("Trump");
  });

  it("keeps different entities as separate chips", () => {
    const chips = searchChips("Trump china", entities);
    expect(chips).toHaveLength(2);
    expect(chips.map((c) => c.entityId)).toEqual([
      "person.trump",
      "country.china",
    ]);
  });

  it("does not false-merge across people", () => {
    const chips = searchChips("Trump Biden", entities);
    expect(chips).toHaveLength(2);
    expect(chips.map((c) => c.entityId)).toEqual([
      "person.trump",
      "person.biden",
    ]);
  });

  it("clear removes all tokens in the group", () => {
    const chips = searchChips("Trump china 特朗普", entities);
    const trump = chips.find((c) => c.entityId === "person.trump")!;
    expect(removeSearchChip("Trump china 特朗普", trump)).toBe("china");
  });
});

describe("mergeAdditiveSearchTerm", () => {
  it("toggles off when same entity already in query", () => {
    expect(mergeAdditiveSearchTerm("Trump", "特朗普", entities)).toBeNull();
    expect(mergeAdditiveSearchTerm("Trump 特朗普", "Trump", entities)).toBeNull();
  });

  it("appends different entity", () => {
    expect(mergeAdditiveSearchTerm("Trump", "china", entities)).toBe(
      "Trump china",
    );
  });

  it("toggles exact unmatched token", () => {
    expect(mergeAdditiveSearchTerm("foo bar", "foo", entities)).toBe("bar");
  });
});
