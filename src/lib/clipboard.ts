/** Copy plain text to the clipboard with a document.execCommand fallback.
 *
 *  `navigator.clipboard.writeText` needs a secure context and can reject
 *  (permissions, older webviews). The textarea + execCommand path covers
 *  those cases so "Copy link" still works in Tauri / HTTP localhost. */
export async function copyText(text: string): Promise<boolean> {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      /* fall through */
    }
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.left = "-9999px";
    ta.style.top = "0";
    document.body.appendChild(ta);
    ta.select();
    ta.setSelectionRange(0, ta.value.length);
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

/** Copy both plain text and HTML when the ClipboardItem API is available, so
 *  pasting into rich editors (Word, Notes, mail) keeps basic formatting.
 *  Falls back to plain text on older webviews or when the write is denied. */
export async function copyRichText(plain: string, html: string): Promise<boolean> {
  if (
    typeof navigator !== "undefined" &&
    typeof ClipboardItem !== "undefined" &&
    navigator.clipboard?.write
  ) {
    try {
      await navigator.clipboard.write([
        new ClipboardItem({
          "text/plain": new Blob([plain], { type: "text/plain" }),
          "text/html": new Blob([html], { type: "text/html" }),
        }),
      ]);
      return true;
    } catch {
      /* fall through to plain text */
    }
  }
  return copyText(plain);
}
