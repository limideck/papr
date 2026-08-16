// Shared helpers for copying / exporting the article currently shown in the
// reader. Clipboard copy uses the unproxied body HTML (not inlined data:
// images) so the payload stays small and links stay useful outside the app.
// Word export resolves/embeds images separately in docx.ts.
//
// Kept free of i18n / feedMeta imports so unit tests can run without a
// browser localStorage polyfill.

export interface ArticleExportSource {
  title: string;
  author: string | null;
  url: string | null;
  feedTitle: string;
  /** Already-formatted published date (locale-aware), if any. */
  publishedLabel?: string | null;
}

/** Escape text for inclusion in an HTML fragment we put on the clipboard. */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Plain, entity-decoded text of an HTML body (same approach as Reader). */
export function htmlToPlainText(html: string): string {
  if (!html) return "";
  return (
    new DOMParser().parseFromString(html, "text/html").body.textContent ?? ""
  );
}

/** Meta lines under the title (author · date · feed · url). */
export function articleMetaLines(a: ArticleExportSource): string[] {
  const lines: string[] = [];
  const bits: string[] = [];
  if (a.author) bits.push(a.author);
  if (a.publishedLabel) bits.push(a.publishedLabel);
  if (a.feedTitle) bits.push(a.feedTitle);
  if (bits.length) lines.push(bits.join(" · "));
  if (a.url) lines.push(a.url);
  return lines;
}

/** Readable plain-text form of the article for the clipboard / previews. */
export function formatArticlePlain(
  a: ArticleExportSource,
  bodyHtml: string,
): string {
  const lines = [a.title, ...articleMetaLines(a)];
  const body = htmlToPlainText(bodyHtml).trim();
  if (body) lines.push("", body);
  return lines.join("\n");
}

/** HTML fragment suitable for rich-text clipboard paste. */
export function formatArticleHtml(
  a: ArticleExportSource,
  bodyHtml: string,
): string {
  const meta = articleMetaLines(a)
    .map((line) => `<p><em>${escapeHtml(line)}</em></p>`)
    .join("");
  const body = bodyHtml.trim() || "<p></p>";
  return `<article><h1>${escapeHtml(a.title)}</h1>${meta}${body}</article>`;
}

/** A filesystem-safe download basename derived from the article title. */
export function articleFilename(title: string, ext: string): string {
  const base =
    title
      .trim()
      .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "")
      .replace(/\s+/g, " ")
      .replace(/[. ]+$/g, "")
      .slice(0, 80) || "article";
  const cleanExt = ext.replace(/^\./, "");
  return `${base}.${cleanExt}`;
}
