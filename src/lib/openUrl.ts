/** Open an external URL in a new browser tab. */
export function openUrl(url: string): void {
  window.open(url, "_blank", "noopener,noreferrer");
}
