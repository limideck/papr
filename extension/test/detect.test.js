/**
 * Unit tests for the extension's pure feed-detection module.
 *
 * `detect.js` is a classic dual-mode script: it has no `import`/`export`, and
 * assigns its API to `module.exports` when a CommonJS `module` is present.
 * `createRequire` lets this ESM test file `require()` it directly.
 */
import { describe, it, expect } from "vitest";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const PaprDetect = require("../src/detect.js");

const {
  detectDeclaredFeeds,
  detectWellKnown,
  extractYoutubeChannelId,
  detectFeeds,
  buildSubscribeLink,
  isYoutubeChannelId,
} = PaprDetect;

describe("detectDeclaredFeeds", () => {
  it("finds an RSS link", () => {
    const feeds = detectDeclaredFeeds(
      [{ rel: "alternate", type: "application/rss+xml", href: "/feed.xml", title: "Blog" }],
      "https://example.com/posts",
    );
    expect(feeds).toHaveLength(1);
    expect(feeds[0].feedUrl).toBe("https://example.com/feed.xml");
    expect(feeds[0].title).toBe("Blog");
    expect(feeds[0].kind).toBe("declared");
  });

  it("finds atom and json feeds", () => {
    const feeds = detectDeclaredFeeds(
      [
        { rel: "alternate", type: "application/atom+xml", href: "https://x.com/atom" },
        { rel: "alternate", type: "application/feed+json", href: "https://x.com/feed.json" },
      ],
      "https://x.com",
    );
    expect(feeds.map((f) => f.feedUrl)).toEqual([
      "https://x.com/atom",
      "https://x.com/feed.json",
    ]);
  });

  it("resolves relative hrefs against the page URL", () => {
    const feeds = detectDeclaredFeeds(
      [{ rel: "alternate", type: "application/rss+xml", href: "../rss" }],
      "https://example.com/blog/posts/",
    );
    expect(feeds[0].feedUrl).toBe("https://example.com/blog/rss");
  });

  it("falls back to the feed URL when no title is given", () => {
    const feeds = detectDeclaredFeeds(
      [{ rel: "alternate", type: "application/rss+xml", href: "https://x.com/feed" }],
      "https://x.com",
    );
    expect(feeds[0].title).toBe("https://x.com/feed");
  });

  it("ignores non-feed alternate links and stylesheets", () => {
    const feeds = detectDeclaredFeeds(
      [
        { rel: "alternate", type: "text/html", href: "https://x.com/amp" },
        { rel: "stylesheet", type: "text/css", href: "https://x.com/app.css" },
        { rel: "alternate", type: "application/rss+xml", href: "https://x.com/feed" },
      ],
      "https://x.com",
    );
    expect(feeds).toHaveLength(1);
    expect(feeds[0].feedUrl).toBe("https://x.com/feed");
  });

  it("matches rel tokens case-insensitively and within a list", () => {
    const feeds = detectDeclaredFeeds(
      [{ rel: "ALTERNATE home", type: "application/rss+xml", href: "https://x.com/feed" }],
      "https://x.com",
    );
    expect(feeds).toHaveLength(1);
  });

  it("deduplicates identical feed URLs", () => {
    const feeds = detectDeclaredFeeds(
      [
        { rel: "alternate", type: "application/rss+xml", href: "https://x.com/feed" },
        { rel: "alternate", type: "application/rss+xml", href: "https://x.com/feed" },
      ],
      "https://x.com",
    );
    expect(feeds).toHaveLength(1);
  });

  it("handles missing or empty input", () => {
    expect(detectDeclaredFeeds(undefined, "https://x.com")).toEqual([]);
    expect(detectDeclaredFeeds([], "https://x.com")).toEqual([]);
  });
});

describe("detectWellKnown — YouTube", () => {
  it("rewrites a /channel/UC… URL to its feed", () => {
    const r = detectWellKnown(
      "https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw",
    );
    expect(r.feedUrl).toBe(
      "https://www.youtube.com/feeds/videos.xml?channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw",
    );
    expect(r.kind).toBe("youtube");
  });

  it("rewrites a playlist URL to its feed", () => {
    const r = detectWellKnown(
      "https://www.youtube.com/playlist?list=PLFgquLnL59alW3xmYiWRaoz0oM3H17Lth",
    );
    expect(r.feedUrl).toBe(
      "https://www.youtube.com/feeds/videos.xml?playlist_id=PLFgquLnL59alW3xmYiWRaoz0oM3H17Lth",
    );
  });

  it("resolves a vanity @handle URL using page HTML", () => {
    const html = '<html><script>{"channelId":"UCXuqSBlHAE6Xw-yeJA0Tunw"}</script></html>';
    const r = detectWellKnown("https://www.youtube.com/@veritasium", html);
    expect(r.feedUrl).toContain("channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw");
  });

  it("returns null for a vanity URL with no resolvable HTML", () => {
    expect(detectWellKnown("https://www.youtube.com/@veritasium")).toBeNull();
  });

  it("leaves an existing feed URL alone", () => {
    expect(
      detectWellKnown(
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw",
      ),
    ).toBeNull();
  });
});

describe("detectWellKnown — Reddit", () => {
  it("rewrites a subreddit URL to its .rss feed", () => {
    const r = detectWellKnown("https://www.reddit.com/r/rust");
    expect(r.feedUrl).toBe("https://www.reddit.com/r/rust/.rss");
    expect(r.kind).toBe("reddit");
  });

  it("preserves a listing variant", () => {
    const r = detectWellKnown("https://www.reddit.com/r/rust/top");
    expect(r.feedUrl).toBe("https://www.reddit.com/r/rust/top/.rss");
  });

  it("ignores a post permalink", () => {
    expect(
      detectWellKnown("https://www.reddit.com/r/rust/comments/abc/title/"),
    ).toBeNull();
  });

  it("ignores the Reddit home page", () => {
    expect(detectWellKnown("https://www.reddit.com/")).toBeNull();
  });
});

describe("detectWellKnown — Mastodon", () => {
  it("rewrites a profile URL to its .rss feed", () => {
    const r = detectWellKnown("https://mastodon.social/@Gargron");
    expect(r.feedUrl).toBe("https://mastodon.social/@Gargron.rss");
    expect(r.kind).toBe("mastodon");
  });

  it("works on a custom instance", () => {
    const r = detectWellKnown("https://hachyderm.io/@nova");
    expect(r.feedUrl).toBe("https://hachyderm.io/@nova.rss");
  });

  it("ignores a single post URL", () => {
    expect(detectWellKnown("https://mastodon.social/@Gargron/109")).toBeNull();
  });
});

describe("detectWellKnown — misc", () => {
  it("returns null for an ordinary site", () => {
    expect(detectWellKnown("https://example.com/about")).toBeNull();
  });

  it("returns null for malformed input", () => {
    expect(detectWellKnown("not a url")).toBeNull();
  });
});

describe("extractYoutubeChannelId", () => {
  it("reads channelId from a JSON blob", () => {
    expect(
      extractYoutubeChannelId('var x={"channelId":"UCXuqSBlHAE6Xw-yeJA0Tunw"}'),
    ).toBe("UCXuqSBlHAE6Xw-yeJA0Tunw");
  });

  it("reads an id from a /channel/ URL substring", () => {
    expect(
      extractYoutubeChannelId(
        '<link rel="canonical" href="https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw">',
      ),
    ).toBe("UCXuqSBlHAE6Xw-yeJA0Tunw");
  });

  it("rejects a malformed id and returns null when absent", () => {
    expect(extractYoutubeChannelId('{"channelId":"UCshort"}')).toBeNull();
    expect(extractYoutubeChannelId("<html>nothing</html>")).toBeNull();
  });
});

describe("isYoutubeChannelId", () => {
  it("accepts a real id and rejects others", () => {
    expect(isYoutubeChannelId("UCXuqSBlHAE6Xw-yeJA0Tunw")).toBe(true);
    expect(isYoutubeChannelId("UCshort")).toBe(false);
    expect(isYoutubeChannelId("PLFgquLnL59alW3xmYiWRaoz0")).toBe(false);
    expect(isYoutubeChannelId(null)).toBe(false);
  });
});

describe("detectFeeds", () => {
  it("combines declared feeds and a well-known pattern", () => {
    const feeds = detectFeeds({
      pageUrl: "https://www.reddit.com/r/rust",
      links: [
        { rel: "alternate", type: "application/rss+xml", href: "https://www.reddit.com/r/rust.rss" },
      ],
    });
    expect(feeds.length).toBeGreaterThanOrEqual(1);
    expect(feeds.some((f) => f.kind === "declared")).toBe(true);
  });

  it("deduplicates a declared feed that equals the well-known feed", () => {
    const feeds = detectFeeds({
      pageUrl: "https://www.reddit.com/r/rust",
      links: [
        { rel: "alternate", type: "application/rss+xml", href: "https://www.reddit.com/r/rust/.rss" },
      ],
    });
    const urls = feeds.map((f) => f.feedUrl);
    expect(new Set(urls).size).toBe(urls.length);
  });

  it("returns an empty list for a plain page with no feeds", () => {
    expect(detectFeeds({ pageUrl: "https://example.com", links: [] })).toEqual([]);
  });

  it("tolerates missing input", () => {
    expect(detectFeeds()).toEqual([]);
    expect(detectFeeds({})).toEqual([]);
  });
});

describe("buildSubscribeLink", () => {
  it("builds an encoded papr:// deep link", () => {
    expect(buildSubscribeLink("https://example.com/feed?a=1&b=2")).toBe(
      "papr://subscribe?url=https%3A%2F%2Fexample.com%2Ffeed%3Fa%3D1%26b%3D2",
    );
  });
});
