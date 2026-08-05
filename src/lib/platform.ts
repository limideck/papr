// Platform helpers for keyboard chrome (⌘ vs Ctrl). Desktop drag-region /
// traffic-light padding is gone — this is a web SPA.

export const isMac =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent || "");

export const modKey = isMac ? "⌘" : "Ctrl";

export const modCombo = (key: string) => (isMac ? `⌘${key}` : `Ctrl+${key}`);
