// Highlight palette (feature F7). The backend stores a colour *key* per
// highlight; the UI resolves it here to a translucent background tint that
// reads as a marker stripe over body text in both light and dark themes.
//
// Distinct from the tag palette (`tagColors.ts`): tags are saturated dots,
// highlights are soft highlighter washes laid behind text.

export interface HighlightColor {
  key: string;
  /** Background wash applied to the <mark>. */
  bg: string;
  /** A stronger swatch colour for the toolbar/picker buttons. */
  swatch: string;
}

export const HIGHLIGHT_COLORS: HighlightColor[] = [
  { key: "yellow", bg: "oklch(0.92 0.13 95 / 0.55)", swatch: "oklch(0.85 0.16 95)" },
  { key: "green", bg: "oklch(0.90 0.12 150 / 0.55)", swatch: "oklch(0.82 0.15 150)" },
  { key: "blue", bg: "oklch(0.90 0.09 240 / 0.55)", swatch: "oklch(0.80 0.12 240)" },
  { key: "pink", bg: "oklch(0.90 0.10 350 / 0.55)", swatch: "oklch(0.82 0.14 350)" },
  { key: "purple", bg: "oklch(0.89 0.10 305 / 0.55)", swatch: "oklch(0.80 0.14 305)" },
];

/** Resolve a stored highlight colour key to its background wash. */
export function highlightBg(key: string): string {
  return (HIGHLIGHT_COLORS.find((c) => c.key === key) ?? HIGHLIGHT_COLORS[0]).bg;
}
