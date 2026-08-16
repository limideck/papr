import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { LANGUAGES } from "../i18n";
import { useUi, PANEL_BOUNDS } from "../store";
import { usePlayer } from "../player";
import { useTranslationJobs } from "../translation";
import { useArticleActions } from "../hooks/articleActions";
import { renderMarkdown } from "../lib/markdown";
import { copyRichText } from "../lib/clipboard";
import { downloadBlob } from "../lib/download";
import {
  articleFilename,
  articleMetaLines,
  formatArticleHtml,
  formatArticlePlain,
} from "../lib/articleExport";
import { articleToDocx } from "../lib/docx";
import { imageDataUrl } from "../lib/imageBytes";
import { fullDate } from "../lib/feedMeta";
import { openUrl } from "../lib/openUrl";
import { reportError, toast } from "../toast";
import { tagColor } from "../lib/tagColors";
import type { ArticleDetail } from "../types";
import Icon from "./Icon";
import TagPicker from "./TagPicker";
import ResizeHandle from "./ResizeHandle";
import HighlightLayer from "./HighlightLayer";
import Lightbox from "./Lightbox";

interface Props {
  onToast: (msg: string) => void;
}

function youtubeId(url: string | null): string | null {
  if (!url) return null;
  const m =
    url.match(/[?&]v=([\w-]{11})/) || url.match(/youtu\.be\/([\w-]{11})/);
  return m ? m[1] : null;
}

function needsImageProxy(src: string): boolean {
  try {
    const host = new URL(src).hostname.toLowerCase();
    return host === "cdnfile.sspai.com" || host === "rssfile.sspai.com";
  } catch {
    return false;
  }
}

/** Plain, entity-decoded text of an HTML body — for the reading-time estimate.
 *  A bare `replace(/<[^>]+>/g, " ")` tag-strip leaves HTML entities intact, so
 *  `Tom &amp; Jerry &mdash; done` would be counted as 5 words / 28 chars when
 *  the real text ("Tom & Jerry — done") is 4 words / 18 chars — inflating the
 *  estimate on entity-heavy articles. Parsing into an inert document decodes
 *  every entity (`&amp;` → `&`, `&mdash;` → `—`) and drops markup cleanly. */
function bodyPlainText(html: string): string {
  if (!html) return "";
  // DOMParser documents are inert — nothing here executes or loads.
  return new DOMParser().parseFromString(html, "text/html").body.textContent ?? "";
}

/** True when the body opens with its own visual media (image / video / iframe)
 *  before any real text. Such an article already leads with a strong visual, so
 *  prepending the list-thumbnail hero on top of it just shows a second, often
 *  unrelated image — the exact complaint in issue #97, where the body starts
 *  with a `<video>` cover while the feed's `media:thumbnail` is a different png.
 *  Walking in document order (media element before the first non-whitespace text
 *  node) is what the plain `body.includes(imageUrl)` guard can't catch, since
 *  the hero image and the body's lead media are distinct URLs here. */
function bodyLeadsWithMedia(html: string): boolean {
  if (!html) return false;
  const doc = new DOMParser().parseFromString(html, "text/html");
  const walker = doc.createTreeWalker(
    doc.body,
    NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT,
  );
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (node.nodeType === Node.TEXT_NODE) {
      if ((node.textContent ?? "").trim().length > 0) return false;
    } else if (node instanceof Element) {
      if (["IMG", "VIDEO", "IFRAME", "PICTURE"].includes(node.tagName)) return true;
    }
  }
  return false;
}

/** CJK ideographs + Japanese kana + Korean Hangul — scripts read by the
 *  character, not the whitespace-delimited word. */
const CJK_CHAR = /[぀-ヿ㐀-鿿가-힯豈-﫿]/u;
/** Global-flagged variant of `CJK_CHAR` for stripping every CJK glyph. */
const CJK_CHAR_GLOBAL = new RegExp(CJK_CHAR.source, "gu");

/** Estimate reading time in minutes for an article body's plain text.
 *
 *  A mixed-script estimate: CJK scripts have no word spacing, so they are
 *  counted by the character (~480 chars/min); latin-script text is counted by
 *  the whitespace-delimited word (~220 wpm). The two contributions are *summed*
 *  — the previous `Math.max(words/220, chars/480)` always lost for English
 *  (a 1000-word article spans ~5500 chars, so `chars/480` ≈ 11 dwarfed the
 *  true `words/220` ≈ 4.5), inflating every latin-script article ~2-3×. */
function estimateReadMinutes(text: string): number {
  let cjkChars = 0;
  for (const ch of text) {
    if (CJK_CHAR.test(ch)) cjkChars++;
  }
  // Words, with CJK characters stripped so they are not also counted as
  // single-character "words" by the latin path.
  const latinWords = text
    .replace(CJK_CHAR_GLOBAL, " ")
    .trim()
    .split(/\s+/)
    .filter(Boolean).length;
  const minutes = cjkChars / 480 + latinWords / 220;
  return Math.max(2, Math.round(minutes));
}

/** Decode a URL fragment, tolerating a malformed `%` escape. A real-world
 *  anchor can carry a literal percent (`#100%-growth`, `#section-50%`), which
 *  is not a valid escape sequence — `decodeURIComponent` throws `URIError` on
 *  it. The bare value still works as an `id` lookup, so fall back to it rather
 *  than letting the throw escape the click handler and kill the link. */
function decodeFragment(frag: string): string {
  try {
    return decodeURIComponent(frag);
  } catch {
    return frag;
  }
}

/** Pull the in-page fragment out of a link click, or null if it isn't one.
 *
 *  Two shapes count as in-page: a bare `#frag` href, and — because the body
 *  HTML is sanitized with the article's URL as the rewrite base — an absolute
 *  `https://site/article#frag` that resolves to the very article being read.
 *  `sourceUrl` is the article's own URL, used to recognise that second case.
 */
function inPageFragment(raw: string, sourceUrl: string | null): string | null {
  if (raw[0] === "#") return decodeFragment(raw.slice(1));
  if (!sourceUrl) return null;
  try {
    const u = new URL(raw);
    const b = new URL(sourceUrl);
    if (u.hash && u.origin === b.origin && u.pathname === b.pathname) {
      return decodeFragment(u.hash.slice(1));
    }
  } catch {
    /* not a parseable absolute URL — treat as external */
  }
  return null;
}

/** Build a click handler for links inside injected HTML (article body, AI
 *  summary). In-page anchor links (footnotes, tables of contents) scroll to
 *  their target within the reader; everything else opens in the external
 *  browser — a bare <a> click would otherwise navigate the Tauri webview away
 *  from the app entirely (or, for a fragment link, to a bogus `app://…#frag`). */
function makeLinkClickHandler(sourceUrl: string | null) {
  return (e: React.MouseEvent) => {
    const link = (e.target as HTMLElement).closest("a");
    if (!link) return;
    const raw = link.getAttribute("href");
    if (!raw) return;
    e.preventDefault();

    const hash = inPageFragment(raw, sourceUrl);
    if (hash != null) {
      if (hash === "") return; // bare `#` — no element to reach
      const root = link.closest(".article-body, .ai-prose");
      // getElementById can't be scoped to the body, so match by id or the
      // legacy `<a name>` form within the rendered content.
      const target = root?.querySelector(
        `[id="${CSS.escape(hash)}"], a[name="${CSS.escape(hash)}"]`,
      );
      target?.scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }

    openUrl(link.href);
  };
}

export default function Reader({ onToast }: Props) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions(toast.error);
  const id = useUi((s) => s.selectedArticleId);
  const focusMode = useUi((s) => s.focusMode);
  const setFocusMode = useUi((s) => s.setFocusMode);
  const aiOpen = useUi((s) => s.aiOpen);
  const setAiOpen = useUi((s) => s.setAiOpen);
  const markReadOnOpen = useUi((s) => s.prefs.markReadOnOpen);
  const markReadOnScroll = useUi((s) => s.prefs.markReadOnScroll);
  const showReadingTime = useUi((s) => s.prefs.showReadingTime);
  const defaultOpenMode = useUi((s) => s.prefs.defaultOpenMode);

  const [scrolled, setScrolled] = useState(false);
  // Which body to show when an extraction exists follows the open mode:
  // "reader" keeps the feed body; "extracted" prefers full text (and may
  // auto-extract on open).
  const [showExtracted, setShowExtracted] = useState(
    defaultOpenMode === "extracted",
  );
  const [showTranslation, setShowTranslation] = useState(false);
  const [tagPick, setTagPick] = useState<{ x: number; y: number } | null>(null);
  // Full-screen image viewer: the article's image srcs + the one to open on
  // (issue #87). Null when closed.
  const [lightbox, setLightbox] = useState<{ srcs: string[]; index: number } | null>(
    null,
  );
  const [heroBroken, setHeroBroken] = useState(false);
  // data: URL of a hero image recovered through the backend after the webview
  // failed to load it directly (see the body-image retry effect below). A data:
  // URL (not blob:) so the bytes stay inline and survive the webview dropping
  // blob backing data under memory pressure — the same fix as body images.
  const [heroDataUrl, setHeroDataUrl] = useState<string | null>(null);
  const [proxiedBody, setProxiedBody] = useState<{
    source: string;
    html: string;
  } | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Article id we already auto-marked read via scroll, so a flurry of scroll
  // events near the foot doesn't fire `setRead` repeatedly before the
  // optimistic cache patch lands.
  const scrollMarkedRef = useRef<number | null>(null);
  const playTrack = usePlayer((s) => s.play);
  const playingSrc = usePlayer((s) => (s.playing ? s.track?.src : null));

  const article = useQuery({
    queryKey: ["article", id],
    queryFn: () => api.getArticle(id as number),
    enabled: id != null,
  });
  const a: ArticleDetail | undefined = article.data;

  // Feed list for per-feed open mode. Shared cache key with the sidebar.
  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  // Effective open mode (issue #110): the feed's own setting, falling back to
  // the global default. `undefined` while the article or feed list is still
  // loading, so the open-mode/auto-extract effects below don't fire before the
  // feed's own setting is known.
  const feedOpenMode =
    a && feeds.data
      ? feeds.data.find((f) => f.id === a.feedId)?.openMode ?? null
      : undefined;
  const openMode =
    feedOpenMode === undefined ? undefined : feedOpenMode ?? defaultOpenMode;

  const readMinutes = useMemo(() => {
    return estimateReadMinutes(bodyPlainText(a?.extractedHtml || a?.contentHtml || ""));
    // Recompute when the body changes — including after full-text extraction
    // replaces the short feed snippet, which keeps the same article id.
  }, [a?.extractedHtml, a?.contentHtml]);

  // Reset scroll + extraction view on article change.
  useEffect(() => {
    setShowExtracted(useUi.getState().prefs.defaultOpenMode === "extracted");
    setShowTranslation(false);
    setScrolled(false);
    setTagPick(null);
    setHeroBroken(false);
    setHeroDataUrl(null);
    scrollMarkedRef.current = null;
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
  }, [id]);

  // Apply the effective open mode once the article (and the feed list) is
  // available — declared after the reset above so it wins the same commit.
  // Applied once per article, so the toolbar toggles keep working afterwards
  // and a feeds refetch never yanks the view back.
  const openModeAppliedRef = useRef<number | null>(null);
  useEffect(() => {
    if (!a || openMode === undefined || openModeAppliedRef.current === a.id)
      return;
    openModeAppliedRef.current = a.id;
    if (openMode === "web" && a.url) openUrl(a.url);
    else setShowExtracted(openMode === "extracted");
  }, [a, openMode]);

  // Recover article-body images the webview fails to load, then hide the
  // stragglers. The webview sends no Referer (see sanitize.rs) — right for
  // blacklist-style hotlink protection (*.sinaimg.cn) but fatal on hosts that
  // *require* one (cdnfile.sspai.com 403s a bare request), and the webview
  // can't vary the value per host. So a failed image gets one retry through
  // the backend, which walks Referer fallbacks (fetch_image) and returns the
  // bytes; the <img> is swapped to an inline data: URL, with the original kept
  // in data-papr-src for the context-menu actions. A data: URL (not blob:) is
  // deliberate — WKWebView/WebView2 silently drop a blob:'s backing data under
  // memory pressure (e.g. layer recompositing while scrolling), so a recovered
  // image carried by a blob: vanishes when the user scrolls away and back; the
  // inline data: bytes always repaint. Images the backend can't recover are
  // hidden — a broken-image icon mid-article is just noise. Runs whenever the
  // body changes (article switch, extract toggle, extraction finishing).
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const pageUrl = a?.url;
    const timers: number[] = [];
    let alive = true;
    const recover = async (img: HTMLImageElement) => {
      const src = img.getAttribute("src") || "";
      if (img.dataset.paprRetried || !/^https?:\/\//.test(src)) {
        img.style.display = "none";
        return;
      }
      img.dataset.paprRetried = "1";
      try {
        const buf = await api.fetchImage(src, pageUrl);
        if (!alive) return;
        img.dataset.paprSrc = src;
        img.src = imageDataUrl(src, buf);
      } catch {
        img.style.display = "none";
      }
    };
    const recoverIfBroken = (img: HTMLImageElement) => {
      if (img.dataset.paprRetried) return;
      if (img.complete && img.naturalWidth === 0) void recover(img);
    };
    const onError = (e: Event) => void recover(e.currentTarget as HTMLImageElement);
    const watched: HTMLImageElement[] = [];
    el.querySelectorAll("img").forEach((img) => {
      img.addEventListener("error", onError);
      watched.push(img);
      // Proxy-eligible images (少数派/CDN hosts that reject a bare no-referrer
      // request) are rewritten to data: URLs up front by the `proxiedBody`
      // effect, so don't fetch them again here — that duplicated the backend /
      // IPC / network work for every matched image. `onError` above stays as a
      // fallback; this only retries an image that had already failed before the
      // listener attached.
      recoverIfBroken(img);
    });
    // WKWebView can finish a parser-inserted image before React's effect
    // listener is attached, and in practice not every broken image reports
    // that state synchronously. A few delayed sweeps make the fallback
    // deterministic without retrying images that are still loading.
    [250, 1000, 2500].forEach((delay) => {
      timers.push(window.setTimeout(() => watched.forEach(recoverIfBroken), delay));
    });
    return () => {
      alive = false;
      timers.forEach(window.clearTimeout);
      watched.forEach((img) => img.removeEventListener("error", onError));
    };
  }, [a?.id, a?.url, showExtracted, a?.extractedHtml, showTranslation, a?.translatedHtml]);

  // Same proactive proxy for the reader hero. These hosts need a Referer that
  // only the Rust fetch path can provide; waiting for `onError` leaves a broken
  // image visible in WKWebView on some builds.
  useEffect(() => {
    if (!a?.imageUrl || heroDataUrl || heroBroken || !needsImageProxy(a.imageUrl)) return;
    let alive = true;
    const articleId = a.id;
    api
      .fetchImage(a.imageUrl, a.url)
      .then((buf) => {
        if (!alive || useUi.getState().selectedArticleId !== articleId) return;
        setHeroDataUrl(imageDataUrl(a.imageUrl!, buf));
      })
      .catch(() => {
        if (!alive || useUi.getState().selectedArticleId !== articleId) return;
        setHeroBroken(true);
      });
    return () => {
      alive = false;
    };
  }, [a?.id, a?.imageUrl, a?.url, heroDataUrl, heroBroken]);

  // Mark as read once when an unread article is opened (if the user opted in).
  useEffect(() => {
    if (a && !a.isRead && markReadOnOpen) actions.setRead(a.id, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id]);

  // The extracted article id travels as the mutation variable, not via the
  // `a` closure: extraction is async and the user can switch articles before
  // it resolves. Keying onSuccess off the live `a` would invalidate the wrong
  // article (the extracted text never shows on return) and toast "full text
  // extracted" while reading an unrelated, un-extracted article.
  const extract = useMutation({
    mutationFn: (articleId: number) => api.extractFulltext(articleId),
    onSuccess: (_data, articleId) => {
      qc.invalidateQueries({ queryKey: ["article", articleId] });
      // Only the article still on screen should flip into the extracted view
      // and surface the toast.
      if (useUi.getState().selectedArticleId === articleId) {
        setShowExtracted(true);
        onToast(t("reader.fullTextExtracted"));
      }
    },
    onError: (e) => reportError(e),
  });

  // The default translation target + engine, switched inline from the toolbar
  // The default target language + engine come from Settings. The toolbar can
  // override them per translation, but only temporarily: the override is not
  // written back and resets to the default when switching articles. The article's
  // cached `translatedLang` (and any running job's `lang`) is compared against the
  // effective `targetLang` to decide whether a translation is current for it.
  const translateSetting = useQuery({
    queryKey: ["setting", "translate_target_lang"],
    queryFn: () => api.getSetting("translate_target_lang"),
  });
  const defaultLang = translateSetting.data || i18n.language;
  const engineSetting = useQuery({
    queryKey: ["setting", "translate_engine"],
    queryFn: () => api.getSetting("translate_engine"),
  });
  const defaultEngine = engineSetting.data || "llm";
  // `null` = follow the default; a string = a temporary per-article override.
  const [tmpLang, setTmpLang] = useState<string | null>(null);
  const [tmpEngine, setTmpEngine] = useState<string | null>(null);
  const targetLang = tmpLang ?? defaultLang;
  const engine = tmpEngine ?? defaultEngine;
  // Drop the temporary overrides when the article changes so each article opens
  // on the configured defaults.
  useEffect(() => {
    setTmpLang(null);
    setTmpEngine(null);
  }, [id]);

  // Background translation jobs run independently of this view, so several
  // articles can translate at once and switching away never interrupts one.
  const startTranslate = useTranslationJobs((s) => s.translate);
  const job = useTranslationJobs((s) => (id != null ? s.jobs[id] : undefined));

  const canTranslate = !!(a?.extractedHtml || a?.contentHtml);
  const baseBody =
    (showExtracted && a?.extractedHtml ? a.extractedHtml : a?.contentHtml) || "";
  const jobForTarget = job && job.lang === targetLang ? job : undefined;
  const translating = jobForTarget?.status === "translating";
  const cachedValid = !!a?.translatedHtml && a.translatedLang === targetLang;
  const translatedBody =
    jobForTarget?.html || (cachedValid ? a?.translatedHtml ?? "" : "");
  const hasTranslation = !!translatedBody;
  const showToggle = hasTranslation || translating;
  const body = showTranslation
    ? translatedBody ||
      (translating ? `<p><em>${t("reader.translating")}</em></p>` : baseBody)
    : baseBody;
  const displayBody = proxiedBody?.source === body ? proxiedBody.html : body;
  // Whether the body already opens with its own image/video — if so, the hero
  // thumbnail is suppressed to avoid a redundant top image (issue #97).
  const leadsWithMedia = useMemo(() => bodyLeadsWithMedia(baseBody), [baseBody]);

  // For hosts that require a Referer (notably 少数派's image CDN), proxy image
  // URLs before injecting the HTML. This avoids relying on WKWebView's image
  // error events for parser-inserted nodes, which are not reliable in release
  // builds. The fetched bytes are inlined as data: URLs (not blob:) so they
  // survive the webview dropping blob backing data while scrolling.
  useEffect(() => {
    if (!body) {
      setProxiedBody(null);
      return;
    }
    const doc = new DOMParser().parseFromString(body, "text/html");
    const imgs = Array.from(doc.body.querySelectorAll("img")).filter((img) => {
      const src = img.getAttribute("src") || "";
      return /^https?:\/\//.test(src) && needsImageProxy(src);
    });
    if (imgs.length === 0) {
      setProxiedBody(null);
      return;
    }

    let alive = true;
    Promise.all(
      imgs.map(async (img) => {
        const src = img.getAttribute("src") || "";
        try {
          const buf = await api.fetchImage(src, a?.url);
          if (!alive) return;
          img.dataset.paprSrc = src;
          img.setAttribute("src", imageDataUrl(src, buf));
          img.removeAttribute("srcset");
          img.removeAttribute("referrerpolicy");
        } catch {
          if (!alive) return;
          img.style.display = "none";
        }
      }),
    ).then(() => {
      if (alive) setProxiedBody({ source: body, html: doc.body.innerHTML });
    });

    return () => {
      alive = false;
    };
  }, [body, a?.url]);

  // When a translation finishes, refetch the article so its persisted
  // `translatedHtml` lands in the cache — the toggle then keeps working after
  // the in-memory job is gone (e.g. reopening the article in a later session).
  useEffect(() => {
    if (id == null || !job) return;
    if (job.status === "done") {
      qc.invalidateQueries({ queryKey: ["article", id] });
    } else if (job.status === "error") {
      // The translation failed (a toast already surfaced why) — drop back to the
      // original so the view isn't stuck on an empty "translating…" state.
      setShowTranslation(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, job?.status]);

  // With an "extracted" open mode in effect, a summary-only feed item is
  // upgraded to the full page the moment it's opened, so the reader never
  // shows a two-line stub. Skipped when the feed already carries the whole
  // article, when there is no source URL to fetch, or once attempted for this
  // article — so a failed fetch isn't retried on every re-render.
  const autoExtractedRef = useRef<number | null>(null);
  useEffect(() => {
    if (openMode !== "extracted" || !a || !a.url || a.extractedHtml) return;
    if (autoExtractedRef.current === a.id || extract.isPending) return;
    // Measure the *decoded* text, not the raw markup. A bare `<[^>]+>` tag
    // strip leaves HTML entities intact, so an entity-heavy stub
    // (`&nbsp;`-padded copy, `&mdash;`/`&amp;` runs) is over-counted — a
    // genuinely short snippet can clear the 800-char bar and wrongly look
    // "complete", leaving the reader showing the very stub auto-extract is
    // meant to replace. `bodyPlainText` decodes entities and drops markup
    // cleanly, the same measurement the reading-time estimate already uses.
    // An explicit per-feed "extracted" skips the bar entirely — the user asked
    // for the page's own text even when the feed body looks complete.
    if (feedOpenMode !== "extracted") {
      const plain = bodyPlainText(a.contentHtml || "").trim();
      if (plain.length >= 800) return; // feed already delivers the full text
    }
    autoExtractedRef.current = a.id;
    extract.mutate(a.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id, a?.extractedHtml, openMode, feedOpenMode]);

  // Mark the current article read once its foot is reached. Also fires for an
  // article short enough to need no scrolling at all (`scrollHeight` already
  // within `clientHeight`) — that case produces no `scroll` event, so without
  // a render-time check a fully-visible short article would never be marked
  // read despite "mark read on scroll" being on.
  const markReadIfAtFoot = useCallback(() => {
    const el = scrollRef.current;
    if (!el || !markReadOnScroll || !a || a.isRead) return;
    if (scrollMarkedRef.current === a.id) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 120) {
      scrollMarkedRef.current = a.id;
      actions.setRead(a.id, true);
    }
  }, [markReadOnScroll, a, actions]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    setScrolled(el.scrollTop > 8);
    markReadIfAtFoot();
  };

  // A short article that fits the viewport never fires `scroll`, so check the
  // foot condition once the body has laid out (article switch, extract toggle,
  // extraction finishing). The check is deferred briefly so body images have a
  // chance to load — measuring `scrollHeight` before they do could read a
  // too-small height and mark a genuinely long article read prematurely. The
  // `scrollMarkedRef` guard keeps it idempotent.
  useEffect(() => {
    const timer = window.setTimeout(markReadIfAtFoot, 400);
    return () => window.clearTimeout(timer);
  }, [markReadIfAtFoot, showExtracted, a?.extractedHtml, a?.contentHtml]);


  // Prefer the translation when one is on screen; otherwise the feed/extracted
  // body. Skip the transient "Translating…" placeholder so copy/export never
  // grab that stub, and use unproxied HTML (no inlined data: images).
  const exportBodyHtml =
    showTranslation && translatedBody ? translatedBody : baseBody;
  const exportSource = a
    ? {
        title: a.title,
        author: a.author,
        url: a.url,
        feedTitle: a.feedTitle,
        publishedLabel: a.publishedAt ? fullDate(a.publishedAt) : null,
      }
    : null;
  const copyArticle = () => {
    if (!exportSource) return;
    const plain = formatArticlePlain(exportSource, exportBodyHtml);
    const html = formatArticleHtml(exportSource, exportBodyHtml);
    void copyRichText(plain, html).then((ok) =>
      onToast(ok ? t("reader.articleCopied") : t("reader.articleCopyFailed")),
    );
  };
  const exportWord = () => {
    if (!exportSource) return;
    // Prefer proxied HTML (already-inlined data: images) when it matches the
    // export body; otherwise resolve remotes via the backend fetch-image path
    // so hotlink-protected hosts still embed in Word.
    const html =
      proxiedBody?.source === exportBodyHtml ? proxiedBody.html : exportBodyHtml;
    void (async () => {
      try {
        const blob = await articleToDocx({
          title: exportSource.title,
          metaLines: articleMetaLines(exportSource),
          bodyHtml: html,
          resolveImage: (src) =>
            api.fetchImage(src, a?.url).catch(() => null),
        });
        downloadBlob(blob, articleFilename(exportSource.title, "docx"));
        onToast(t("reader.wordExported"));
      } catch (e) {
        reportError(e);
        onToast(t("reader.wordExportFailed"));
      }
    })();
  };
  // Article-body clicks: an image opens the full-screen viewer (issue #87), with
  // the article's other images available for ← / → navigation; anything else
  // falls through to the link handler (in-page anchors, external links).
  const linkClick = makeLinkClickHandler(a?.url ?? null);
  const handleBodyClick = (e: React.MouseEvent) => {
    const img = (e.target as HTMLElement).closest("img") as HTMLImageElement | null;
    const root = bodyRef.current;
    if (img && root?.contains(img)) {
      // Use the src the DOM actually renders — a proxied data: URL when the
      // original hotlink-protected host needed a Referer — and skip hidden /
      // broken images so the gallery matches what the reader shows.
      const imgs = Array.from(
        root.querySelectorAll<HTMLImageElement>("img"),
      ).filter(
        (im) => im.style.display !== "none" && (im.currentSrc || im.getAttribute("src")),
      );
      const index = imgs.indexOf(img);
      if (index >= 0) {
        e.preventDefault();
        setLightbox({ srcs: imgs.map((im) => im.currentSrc || im.src), index });
        return;
      }
    }
    linkClick(e);
  };

  if (id == null) {
    const kbd = {
      fontFamily: "var(--mono)",
      fontSize: 10,
      padding: "1px 5px",
      border: "1px solid var(--hair)",
      borderRadius: 3,
    };
    return (
      <div className="reader" role="main">
        <div className="reader-toolbar" />
        <div className="empty" style={{ flex: 1 }}>
          <div className="glyph">
            <Icon name="rss" size={22} />
          </div>
          <div>{t("reader.emptySelectArticle")}</div>
          <div style={{ fontSize: 11.5, color: "var(--muted-2)" }}>
            {t("reader.emptyHintPrefix")} <kbd style={kbd}>J</kbd> /{" "}
            <kbd style={kbd}>K</kbd> {t("reader.emptyHintSuffix")}
          </div>
        </div>
      </div>
    );
  }

  // An article is selected but its detail isn't loaded yet — still fetching
  // or the fetch failed. Surface that explicitly instead of falling through
  // to the "select an article" empty state, which would be misleading.
  if (!a) {
    return (
      <div className="reader" role="main">
        <div className="reader-toolbar" />
        {article.isError ? (
          <div className="empty" style={{ flex: 1 }}>
            <div className="glyph">
              <Icon name="alert" size={22} />
            </div>
            <div>{t("reader.loadError")}</div>
            <button
              className="empty-retry"
              onClick={() => article.refetch()}
              disabled={article.isFetching}
            >
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        ) : (
          <div className="reader-scroll">
            <div className="article reader-content" aria-hidden="true">
              <div className="sk-line" style={{ width: "28%" }} />
              <div
                className="sk-line"
                style={{ width: "82%", height: 24, marginBottom: 18 }}
              />
              <div
                className="sk-line"
                style={{ width: "44%", marginBottom: 30 }}
              />
              {Array.from({ length: 9 }).map((_, i) => (
                <div
                  key={i}
                  className="sk-line"
                  style={{ width: i % 3 === 2 ? "58%" : "100%", height: 12 }}
                />
              ))}
            </div>
          </div>
        )}
      </div>
    );
  }

  // `canTranslate`, `body`, `displayBody` and the translation state are
  // computed above (before the early returns) so the image-proxy and
  // recovery effects can depend on them.

  // Translate into `lang` with `eng` and show the result. Defaults come from
  // Settings; the toolbar passes temporary overrides here (not persisted). Always
  // starts a fresh job (the store skips only a duplicate in-flight one).
  const run = (lang: string, eng: string) => {
    if (!canTranslate) return;
    startTranslate(a.id, lang, eng);
    setShowTranslation(true);
  };

  const ytId = a.sourceType === "youtube" ? youtubeId(a.url) : null;

  return (
    <div className={`reader ${aiOpen ? "ai-open" : ""}`} role="main">
      <div className={`reader-toolbar ${scrolled ? "scrolled" : ""}`}>
        <button
          className={`tb-btn ${a.isStarred ? "on" : ""}`}
          onClick={() => actions.setStarred(a.id, !a.isStarred)}
          title={t("reader.tbStar")}
          aria-label={t("reader.tbStar")}
          aria-pressed={a.isStarred}
        >
          <Icon name={a.isStarred ? "star-fill" : "star"} size={16} />
        </button>
        <button
          className={`tb-btn ${a.readLater ? "on" : ""}`}
          onClick={() => actions.setReadLater(a.id, !a.readLater)}
          title={t("reader.tbReadLater")}
          aria-label={t("reader.tbReadLater")}
          aria-pressed={a.readLater}
        >
          <Icon name={a.readLater ? "bookmark-fill" : "bookmark"} size={16} />
        </button>
        <button
          className={`tb-btn ${a.tags.length > 0 ? "on" : ""}`}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setTagPick((p) => (p ? null : { x: r.left, y: r.bottom + 6 }));
          }}
          title={t("reader.tbTags")}
          aria-label={t("reader.tbTags")}
          aria-haspopup="menu"
          aria-expanded={tagPick != null}
        >
          <Icon name="tag" size={16} />
        </button>
        <button
          className="tb-btn"
          title={t("reader.tbCopyArticle")}
          aria-label={t("reader.tbCopyArticle")}
          onClick={copyArticle}
        >
          <Icon name="copy" size={16} />
        </button>
        <button
          className="tb-btn"
          title={t("reader.tbExportWord")}
          aria-label={t("reader.tbExportWord")}
          onClick={exportWord}
        >
          <Icon name="file-text" size={16} />
        </button>
        <HighlightLayer
          // Keyed by article id so the export menu / popovers reset cleanly
          // when the reader switches articles.
          key={a.id}
          articleId={a.id}
          bodyRef={bodyRef}
          bodyVersion={displayBody}
        />
        <div className="tb-btn spacer" />
        <button
          className={`tb-btn ${aiOpen ? "on" : ""}`}
          title={t("reader.tbAiSummary")}
          aria-label={t("reader.tbAiSummary")}
          aria-pressed={aiOpen}
          onClick={() => setAiOpen(!aiOpen)}
        >
          <Icon name={aiOpen ? "sparkle-fill" : "sparkle"} size={16} />
        </button>
        <button
          className={`tb-btn ${showTranslation ? "on" : ""}`}
          title={t("reader.tbTranslate")}
          aria-label={t("reader.tbTranslate")}
          aria-pressed={showTranslation}
          disabled={!canTranslate}
          onClick={() =>
            // One click: translate with the default language + engine, or flip
            // back to the original. Switch engine/language inline on the toggle.
            showTranslation ? setShowTranslation(false) : run(targetLang, engine)
          }
        >
          <Icon name="globe" size={16} />
        </button>
        <button
          className={`tb-btn ${focusMode ? "on" : ""}`}
          title={t("reader.tbFocusMode")}
          aria-label={t("reader.tbFocusMode")}
          aria-pressed={focusMode}
          onClick={() => setFocusMode(!focusMode)}
        >
          <Icon name={focusMode ? "eye-off" : "focus"} size={16} />
        </button>
        {a.url && (
          <button
            className="tb-btn"
            title={t("reader.tbOpenInBrowser")}
            aria-label={t("reader.tbOpenInBrowser")}
            onClick={() => openUrl(a.url!)}
          >
            <Icon name="open" size={16} />
          </button>
        )}
      </div>

      {/* Article + AI summary share a row so the drawer reserves width instead
          of overlaying the body (tags, images, text). */}
      <div className="reader-main">
      <div
        className="reader-scroll"
        ref={scrollRef}
        onScroll={onScroll}
      >
        <article className="article reader-content" key={a.id}>
          <button
            type="button"
            className="article-feed"
            title={t("reader.viewAllFromFeed")}
            onClick={() =>
              useUi.getState().select({ kind: "feed", value: a.feedId }, a.feedTitle)
            }
          >
            <Icon name="rss" size={13} />
            {a.feedTitle}
          </button>
          <h1 className="article-title">{a.title}</h1>
          <div className="article-meta">
            {a.author && <span className="author">{a.author}</span>}
            {a.author && a.publishedAt && <span>·</span>}
            {a.publishedAt && <span>{fullDate(a.publishedAt)}</span>}
            {showReadingTime && (
              <>
                <span>·</span>
                <span>{t("reader.readMinutes", { count: readMinutes })}</span>
              </>
            )}
            {extract.isPending && (
              <>
                <span>·</span>
                <span>{t("reader.extractingFullText")}</span>
              </>
            )}
          </div>

          {a.tags.length > 0 && (
            <div className="article-tags">
              {a.tags.map((tag) => (
                <button
                  key={tag.id}
                  className={`article-tag${tag.kind === "ai" ? " ai" : ""}`}
                  style={{ "--tag-c": tagColor(tag.color) } as React.CSSProperties}
                  title={
                    tag.kind === "ai"
                      ? t("reader.aiTagHint")
                      : t("reader.interestTagHint")
                  }
                  onClick={() =>
                    useUi.getState().select({ kind: "tag", value: tag.id }, tag.name)
                  }
                >
                  <span className="tag-dot" />
                  {tag.name}
                </button>
              ))}
            </div>
          )}

          {ytId ? (
            <iframe
              style={{ width: "100%", aspectRatio: "16 / 9" }}
              // Privacy-enhanced host: YouTube sets no tracking cookies
              // until the viewer actually starts the video.
              src={`https://www.youtube-nocookie.com/embed/${ytId}`}
              title={a.title}
              referrerPolicy="strict-origin-when-cross-origin"
              allowFullScreen
            />
          ) : (
            a.imageUrl &&
            !heroBroken &&
            // Skip the hero when the body already embeds the same image, so
            // feeds that repeat their lead image don't show it twice.
            !body.includes(a.imageUrl) &&
            // Also skip it when the body opens with its own image/video — the
            // article already leads with a visual, so a separate thumbnail on
            // top would just be a redundant (often mismatched) image (#97).
            !leadsWithMedia && (
              <img
                className="reader-hero"
                src={heroDataUrl ?? a.imageUrl}
                alt=""
                // The original URL when src is a recovered data: URL, so the
                // context-menu copy/save actions see a real address.
                data-papr-src={heroDataUrl ? a.imageUrl : undefined}
                // No Referer, for the same hotlink-protection reason feed-body
                // images are sanitized this way (e.g. *.sinaimg.cn 403s a
                // request carrying our origin). See `sanitize`.
                referrerPolicy="no-referrer"
                // Same recovery as body images: hosts that *require* a Referer
                // (cdnfile.sspai.com) 403 the direct load, so retry through
                // the backend's Referer-fallback fetch before giving up.
                onError={() => {
                  // A failing data: URL means the recovered bytes weren't a
                  // renderable image — don't loop, give up.
                  if (heroDataUrl) {
                    setHeroBroken(true);
                    return;
                  }
                  const articleId = a.id;
                  api
                    .fetchImage(a.imageUrl!, a.url)
                    .then((buf) => {
                      if (useUi.getState().selectedArticleId !== articleId) return;
                      setHeroDataUrl(imageDataUrl(a.imageUrl!, buf));
                    })
                    .catch(() => {
                      if (useUi.getState().selectedArticleId !== articleId) return;
                      setHeroBroken(true);
                    });
                }}
              />
            )
          )}

          {a.enclosures
            .filter((e) => e.mimeType?.startsWith("audio"))
            .map((e, i) => {
              const isPlaying = playingSrc === e.url;
              return (
                <button
                  className={`episode ${isPlaying ? "playing" : ""}`}
                  key={`a${i}`}
                  onClick={() =>
                    playTrack({
                      articleId: a.id,
                      title: a.title,
                      feedTitle: a.feedTitle,
                      src: e.url,
                    })
                  }
                >
                  <span className="episode-play">
                    <Icon name={isPlaying ? "pause" : "play"} size={15} />
                  </span>
                  <span className="episode-text">
                    {isPlaying
                      ? t("reader.episodePlaying")
                      : t("reader.episodePlay")}
                  </span>
                </button>
              );
            })}
          {a.enclosures
            .filter((e) => e.mimeType?.startsWith("video"))
            .map((e, i) => (
              <div className="enclosure" key={`v${i}`}>
                <video controls src={e.url} />
              </div>
            ))}

          {showToggle && (
            <div className="tr-toggle" role="group" aria-label={t("reader.tbTranslate")}>
              <button
                className={!showTranslation ? "on" : ""}
                aria-pressed={!showTranslation}
                onClick={() => setShowTranslation(false)}
              >
                {t("reader.original")}
              </button>
              <button
                className={showTranslation ? "on" : ""}
                aria-pressed={showTranslation}
                onClick={() => setShowTranslation(true)}
              >
                {t("reader.translation")}
              </button>
              {/* Temporary per-article switchers — change engine or language for
                  this translation only, without touching the configured defaults;
                  switching re-translates with the new choice straight away. */}
              <select
                className="s-select tr-sel"
                value={engine}
                aria-label={t("reader.translateEngine")}
                onChange={(e) => {
                  setTmpEngine(e.target.value);
                  run(targetLang, e.target.value);
                }}
              >
                <option value="llm">{t("reader.translateEngineLlm")}</option>
                <option value="google">Google</option>
                <option value="deepl">DeepL</option>
                <option value="bing">Bing</option>
              </select>
              <select
                className="s-select tr-sel"
                value={targetLang}
                aria-label={t("reader.translateTitle")}
                onChange={(e) => {
                  setTmpLang(e.target.value);
                  run(e.target.value, engine);
                }}
              >
                {LANGUAGES.map((l) => (
                  <option key={l.code} value={l.code}>
                    {l.label}
                  </option>
                ))}
              </select>
              {translating && (
                <span className="tr-progress">
                  {t("reader.translating")}
                  {jobForTarget && jobForTarget.total > 0 &&
                    ` ${jobForTarget.done}/${jobForTarget.total}`}
                </span>
              )}
            </div>
          )}

          <div
            className="article-body"
            ref={bodyRef}
            onClick={handleBodyClick}
            dangerouslySetInnerHTML={{
              __html: displayBody || `<p><em>${t("reader.noContent")}</em></p>`,
            }}
          />
        </article>
      </div>

      <AIDrawer
        // Keyed by article id so switching articles remounts the drawer:
        // its `text` state then re-initialises from the new article's
        // summary, rather than carrying the previous one's across.
        key={a.id}
        open={aiOpen}
        article={a}
        onClose={() => setAiOpen(false)}
      />
      </div>

      {lightbox && (
        <Lightbox
          srcs={lightbox.srcs}
          index={lightbox.index}
          onClose={() => setLightbox(null)}
        />
      )}

      {tagPick && (
        <TagPicker
          articleId={a.id}
          attachedTags={a.tags}
          x={tagPick.x}
          y={tagPick.y}
          onClose={() => setTagPick(null)}
        />
      )}
    </div>
  );
}

function AIDrawer({
  open,
  article,
  onClose,
}: {
  open: boolean;
  article: ArticleDetail;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const aiWidth = useUi((s) => s.aiWidth);
  // Initialised from the article's stored summary (if any). The parent keys
  // this component by article id, so a switch remounts it and re-runs this
  // initialiser — no separate "reset on article change" effect is needed.
  const [text, setText] = useState<string | null>(article.aiSummary);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  // Identifies the latest summarize run. Closing the drawer mid-stream cancels
  // an effect run but the component stays mounted (it is only moved off-screen),
  // so the underlying request keeps streaming and its promise settles later.
  // Only the run whose generation still matches may touch `busy` on settle —
  // otherwise a stale run's `finally` would either wedge the drawer on the
  // loading state or clobber a newer run's `busy` flag.
  const runRef = useRef(0);

  // Generate a summary the first time the drawer opens for an article, and
  // again whenever the user hits Retry. `failed` is in the guard so a failed
  // run isn't silently re-attempted just because the drawer was reopened.
  useEffect(() => {
    if (!open || busy || text || failed) return;
    const run = ++runRef.current;
    let cancelled = false;
    // Whether the stream settled (resolved or rejected) on its own. If the
    // cleanup runs while this is still false, the drawer was closed mid-stream
    // — the accumulated `text` is then a truncated fragment.
    let settled = false;
    // An error raised inside the stream surfaces twice: once as an `error`
    // channel event (carrying the precise provider message) and again as the
    // command's rejected promise. Toast only the first so the user does not
    // see the same failure reported twice; the `.catch` still toasts for
    // failures that abort before streaming starts (no key, bad config) and so
    // never emit an `error` event.
    let sawErrorEvent = false;
    setBusy(true);
    setText("");
    api
      .aiSummarize(article.id, (ev) => {
        if (cancelled) return;
        if (ev.type === "delta") setText((s) => (s ?? "") + ev.data);
        else if (ev.type === "error") {
          sawErrorEvent = true;
          setFailed(true);
          toast.error(ev.data);
        }
      })
      .then(() => {
        if (!cancelled) qc.invalidateQueries({ queryKey: ["article", article.id] });
      })
      .catch((e) => {
        if (!cancelled && !sawErrorEvent) {
          setFailed(true);
          reportError(e);
        }
      })
      .finally(() => {
        settled = true;
        // Clear `busy` for the current run even if it was cancelled — the
        // component is still mounted, and leaving `busy` true would wedge the
        // drawer on the loading state. Skip if a newer run has superseded us.
        if (runRef.current === run) setBusy(false);
      });
    return () => {
      cancelled = true;
      // Closed mid-stream: the backend discards an interrupted generation
      // (it is never persisted), so drop the partial fragment held here too.
      // Reopening then re-generates from scratch instead of showing — and
      // permanently freezing on — a truncated half-summary.
      if (!settled) setText(article.aiSummary);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, article.id, retry]);

  const loading = busy && !text;
  const onRetry = () => {
    setText("");
    setFailed(false);
    setRetry((n) => n + 1);
  };
  // Parse + sanitize the summary only when the text changes, not on every
  // AIDrawer re-render (e.g. each open/close toggle).
  const html = useMemo(() => (text ? renderMarkdown(text) : ""), [text]);

  return (
    <div
      className={`ai-drawer ${open ? "open" : ""}`}
      // A labelled complementary landmark so screen-reader users can jump
      // straight to the summary.
      role="complementary"
      aria-label={t("reader.aiSummaryTitle")}
      // When closed the drawer is only moved off-screen — `inert` keeps its
      // close button and content out of the tab order and the a11y tree.
      inert={!open}
    >
      {/* Left-edge handle: dragging left widens the drawer. Hidden from the
          a11y tree while the drawer is closed (the whole drawer is `inert`). */}
      <div className="resize-handle-slot resize-handle-slot--inline">
        <ResizeHandle
          width={aiWidth}
          side="left"
          min={PANEL_BOUNDS.ai.min}
          max={PANEL_BOUNDS.ai.max}
          onResize={(w) => useUi.getState().setPanel({ aiWidth: w })}
          label={t("reader.resizeAi")}
        />
      </div>
      <div className="ai-head">
        <span className="accent-ico">
          <Icon name="sparkle-fill" size={15} />
        </span>
        <h3>{t("reader.aiSummaryTitle")}</h3>
        <button
          className="tb-btn close"
          onClick={onClose}
          title={t("common.close")}
          aria-label={t("common.close")}
        >
          <Icon name="x" size={14} />
        </button>
      </div>
      <div className="ai-body" aria-live="polite" aria-busy={busy}>
        {loading && (
          <div className="ai-loading">
            <span className="ai-dot" />
            <span className="ai-dot" />
            <span className="ai-dot" />
            <span style={{ marginLeft: 4 }}>{t("reader.aiReadingFullText")}</span>
          </div>
        )}
        {failed && !busy && (
          <div className="ai-error">
            <Icon name="alert" size={18} />
            <span>{t("reader.aiError")}</span>
            <button className="empty-retry" onClick={onRetry}>
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        )}
        {text && !failed && (
          <>
            <div
              className="ai-prose"
              onClick={makeLinkClickHandler(article.url)}
              dangerouslySetInnerHTML={{ __html: html }}
            />
            <div
              style={{
                fontSize: 11,
                color: "var(--muted-2)",
                marginTop: 24,
                lineHeight: 1.5,
              }}
            >
              {t("reader.aiDisclaimer")}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
