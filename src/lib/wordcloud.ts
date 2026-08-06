export type WordCloudGroup =
  | "country"
  | "person"
  | "location"
  | "military"
  | "politics"
  | "economy"
  | "disaster"
  | "org"
  | "general";

export const WORD_CLOUD_GROUPS: WordCloudGroup[] = [
  "country",
  "person",
  "location",
  "military",
  "politics",
  "economy",
  "disaster",
  "org",
  "general",
];

export const GROUP_COLORS: Record<WordCloudGroup, string> = {
  country: "#ef4444",
  person: "#a855f7",
  location: "#22c55e",
  military: "#f97316",
  politics: "#06b6d4",
  economy: "#eab308",
  disaster: "#ec4899",
  org: "#6366f1",
  general: "#60a5fa",
};

export function asWordCloudGroup(raw: string | undefined | null): WordCloudGroup {
  const g = (raw ?? "general").toLowerCase();
  return (WORD_CLOUD_GROUPS as string[]).includes(g)
    ? (g as WordCloudGroup)
    : "general";
}
