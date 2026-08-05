/**
 * Feed detection for the Papr browser extension.
 *
 * This module is deliberately pure and dependency-free so it works in two
 * environments with no build step:
 *   1. as a CommonJS module imported by the Node/vitest test suite, and
 *   2. as a plain classic script loaded by the extension's content script,
 *      where it hangs its API off `globalThis.PaprDetect`.
 *
 * It contains no `import`/`export` keywords (which would be a syntax error in
 * a classic browser script) — the dual-mode wiring at the bottom uses a
 * runtime `typeof module` check instead.
 *
 * It detects:
 *   - declared feeds: `<link rel="alternate" type="application/rss+xml|
 *     atom+xml|json">` in the page <head>;
 *   - well-known source patterns (YouTube channels, Reddit subreddits,
 *     Mastodon profiles) — mirroring the desktop app's F5 normalization.
 *
 * Every function takes its inputs explicitly (no `document`/`window` access)
 * so the same code runs unchanged under Node and inside a tab.
 */
(function (root, factory) {
  const api = factory();
  // Classic-script / extension use: expose a global.
  if (typeof globalThis !== "undefined") {
    globalThis.PaprDetect = api;
  }
  // Node / vitest use: CommonJS export.
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})(this, function () {
  "use strict";

  /** MIME types that mark a `<link rel="alternate">` as a feed. */
  const FEED_TYPES = [
    "application/rss+xml",
    "application/atom+xml",
    "application/feed+json",
    "application/json",
  ];

  /**
   * Resolve a possibly-relative `href` against a base URL.
   * @returns {string} an absolute URL, or `href` unchanged on failure.
   */
  function resolveUrl(href, baseUrl) {
    try {
      return new URL(href, baseUrl).toString();
    } catch (_) {
      return href;
    }
  }

  /**
   * Extract declared feed links from a list of `<link>`-like descriptors.
   * Each descriptor is `{ rel, type, href, title }` — exactly what a content
   * script reads off the DOM, and what tests pass in directly.
   */
  function detectDeclaredFeeds(links, pageUrl) {
    const out = [];
    const seen = new Set();
    for (const link of links || []) {
      const rel = (link.rel || "").toLowerCase();
      const type = (link.type || "").toLowerCase().trim();
      if (!rel.split(/\s+/).includes("alternate")) continue;
      if (FEED_TYPES.indexOf(type) === -1) continue;
      if (!link.href) continue;
      const feedUrl = resolveUrl(link.href, pageUrl);
      if (seen.has(feedUrl)) continue;
      seen.add(feedUrl);
      out.push({
        title: (link.title || "").trim() || feedUrl,
        feedUrl: feedUrl,
        kind: "declared",
      });
    }
    return out;
  }

  /** Reddit listing path segments that have their own `.rss` feed. */
  const REDDIT_LISTINGS = ["hot", "new", "top", "rising"];

  /**
   * True if `hostname` is `youtube.com` or one of its subdomains. Anchored on
   * a dot boundary so a lookalike host (`notyoutube.com`) is rejected.
   */
  function isYoutubeHost(hostname) {
    const h = (hostname || "").toLowerCase();
    return h === "youtube.com" || h.endsWith(".youtube.com");
  }

  /** Build the channel-feed result object for a `UC…` channel id. */
  function youtubeChannelFeed(channelId) {
    return {
      title: "YouTube channel",
      feedUrl:
        "https://www.youtube.com/feeds/videos.xml?channel_id=" + channelId,
      kind: "youtube",
    };
  }

  /** True if `id` looks like a YouTube channel id (`UC` + 22 chars). */
  function isYoutubeChannelId(id) {
    return (
      typeof id === "string" &&
      id.length === 24 &&
      id.indexOf("UC") === 0 &&
      /^[A-Za-z0-9_-]+$/.test(id)
    );
  }

  /**
   * Pull a YouTube channel id (`UC…`) out of channel-page HTML — used to
   * resolve vanity URLs. Mirrors the desktop app's `extract_channel_id`.
   */
  function extractYoutubeChannelId(html) {
    if (typeof html !== "string") return null;
    const keys = ['"channelId":"', '"externalId":"', '"externalChannelId":"'];
    for (const key of keys) {
      const start = html.indexOf(key);
      if (start === -1) continue;
      const rest = html.slice(start + key.length);
      const end = rest.indexOf('"');
      if (end === -1) continue;
      const id = rest.slice(0, end);
      if (isYoutubeChannelId(id)) return id;
    }
    const idx = html.indexOf("/channel/");
    if (idx !== -1) {
      const m = html.slice(idx + "/channel/".length).match(/^[A-Za-z0-9_-]+/);
      if (m && isYoutubeChannelId(m[0])) return m[0];
    }
    return null;
  }

  /**
   * Detect a well-known source feed from the page URL alone (no DOM needed for
   * the path-based cases). `pageHtml` is optional and only consulted to
   * resolve a YouTube vanity URL (`@handle`, `/c/`, `/user/`).
   */
  function detectWellKnown(pageUrl, pageHtml) {
    let url;
    try {
      url = new URL(pageUrl);
    } catch (_) {
      return null;
    }
    const host = url.hostname.toLowerCase();
    const segments = url.pathname.split("/").filter(Boolean);

    // ── YouTube ──
    if (isYoutubeHost(host)) {
      if (url.pathname.indexOf("/feeds/videos.xml") !== -1) return null;
      if (segments[0] === "channel" && isYoutubeChannelId(segments[1])) {
        return youtubeChannelFeed(segments[1]);
      }
      if (segments[0] === "playlist") {
        const list = url.searchParams.get("list");
        if (list && /^[A-Za-z0-9_-]{13,}$/.test(list)) {
          return {
            title: "YouTube playlist",
            feedUrl:
              "https://www.youtube.com/feeds/videos.xml?playlist_id=" + list,
            kind: "youtube",
          };
        }
      }
      const isVanity =
        !!segments[0] &&
        (segments[0].charAt(0) === "@" ||
          segments[0] === "c" ||
          segments[0] === "user");
      if (isVanity && pageHtml) {
        const id = extractYoutubeChannelId(pageHtml);
        if (id) return youtubeChannelFeed(id);
      }
      return null;
    }

    // ── Reddit ──
    if (host === "reddit.com" || host.endsWith(".reddit.com")) {
      if (
        segments[0] === "r" &&
        segments[1] &&
        /^[A-Za-z0-9_]+$/.test(segments[1])
      ) {
        const sub = segments[1];
        const listing = segments[2];
        if (!listing) {
          return {
            title: "r/" + sub,
            feedUrl: "https://www.reddit.com/r/" + sub + "/.rss",
            kind: "reddit",
          };
        }
        if (REDDIT_LISTINGS.indexOf(listing) !== -1) {
          return {
            title: "r/" + sub + "/" + listing,
            feedUrl:
              "https://www.reddit.com/r/" + sub + "/" + listing + "/.rss",
            kind: "reddit",
          };
        }
      }
      return null;
    }

    // ── Mastodon: a profile is exactly one "@"-prefixed path segment. ──
    if (
      segments.length === 1 &&
      segments[0].charAt(0) === "@" &&
      segments[0].length > 1
    ) {
      if (segments[0].slice(-4) === ".rss") return null;
      return {
        title: segments[0],
        feedUrl: url.protocol + "//" + host + "/" + segments[0] + ".rss",
        kind: "mastodon",
      };
    }

    return null;
  }

  /**
   * Top-level detection: combine declared feeds and well-known patterns into
   * a single deduplicated list. This is what the popup renders.
   *
   * @param {{pageUrl:string,links?:Array,pageHtml?:string}} input
   * @returns {Array<{title:string,feedUrl:string,kind:string}>}
   */
  function detectFeeds(input) {
    const cfg = input || {};
    const results = [];
    const seen = new Set();
    const push = function (item) {
      if (item && !seen.has(item.feedUrl)) {
        seen.add(item.feedUrl);
        results.push(item);
      }
    };
    const declared = detectDeclaredFeeds(cfg.links, cfg.pageUrl);
    for (const f of declared) push(f);
    push(detectWellKnown(cfg.pageUrl, cfg.pageHtml));
    return results;
  }

  /** Build a `papr://subscribe?url=…` deep link for a detected feed URL. */
  function buildSubscribeLink(feedUrl) {
    return "papr://subscribe?url=" + encodeURIComponent(feedUrl);
  }

  return {
    FEED_TYPES: FEED_TYPES,
    detectDeclaredFeeds: detectDeclaredFeeds,
    detectWellKnown: detectWellKnown,
    extractYoutubeChannelId: extractYoutubeChannelId,
    detectFeeds: detectFeeds,
    buildSubscribeLink: buildSubscribeLink,
    isYoutubeChannelId: isYoutubeChannelId,
    isYoutubeHost: isYoutubeHost,
  };
});
