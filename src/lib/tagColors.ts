// Tag palette. The backend stores a colour *key* per tag (cycled on create);
// the UI resolves it to an OKLCH value here so chips read consistently in
// both light and dark themes.

export const TAG_PALETTE: Record<string, string> = {
  clay: "oklch(0.64 0.13 38)",
  amber: "oklch(0.70 0.13 75)",
  pine: "oklch(0.58 0.10 165)",
  teal: "oklch(0.62 0.09 200)",
  indigo: "oklch(0.58 0.14 268)",
  violet: "oklch(0.58 0.16 300)",
  rose: "oklch(0.62 0.16 12)",
  slate: "oklch(0.55 0.03 250)",
};

/** Resolve a stored tag colour key to a CSS colour (falls back to clay). */
export function tagColor(key: string): string {
  return TAG_PALETTE[key] ?? TAG_PALETTE.clay;
}
