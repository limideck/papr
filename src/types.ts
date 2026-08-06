// Type mirrors of the Rust / HTTP domain model.

/** Logged-in session user from `GET /api/auth/me`. */
export interface SessionUser {
  id: number;
  username: string;
  isAdmin: boolean;
}

/** Admin user-management row from `GET /api/users`. */
export interface ManagedUser {
  id: number;
  username: string;
  isAdmin: boolean;
  createdAt?: string;
}

/** Queue snapshot from `GET /api/auto-tag/status` (shape may grow). */
export interface AutoTagStatus {
  pending?: number;
  processing?: number;
  failed?: number;
  done?: number;
  /** True when interest matching and/or AI tagging is enabled. */
  enabled?: boolean;
  interestEnabled?: boolean;
  aiEnabled?: boolean;
  lastError?: string | null;
  recentErrors?: { articleId?: number; error?: string; at?: string }[];
}

/** A directory-index feed source (admin-managed). */
export interface FeedSource {
  id: number;
  baseUrl: string;
  lastCheckedAt: string | null;
  feedCount: number;
  /** Auto-created folder that holds feeds from this index. */
  folderId?: number | null;
  folderName?: string | null;
}

export interface FeedSourceScanResult {
  addedCount: number;
  skipped: number;
  added?: { url: string; name?: string }[];
  stale?: { id: string | number; name: string; url: string; reason?: string }[];
}

export interface WordCloudTerm {
  term: string;
  count: number;
  /** Entity category for colouring; defaults to general. */
  group?: string;
}

export interface WordCloudResult {
  terms: WordCloudTerm[];
  /** Article rows loaded for the date range (not the term count). */
  scanned?: number;
  from?: string;
  to?: string;
}

export interface WordCloudStopwords {
  version: number;
  words: string[];
}

export interface WordCloudEntity {
  id: string;
  canonical: string;
  group: string;
  aliases: string[];
}

export interface WordCloudEntities {
  version: number;
  entities: WordCloudEntity[];
}

export type SourceType =
  | "rss"
  | "youtube"
  | "podcast"
  | "mastodon"
  | "bluesky"
  | "reddit"
  | "newsletter";

/** A configured email-newsletter source (mirrors commands::NewsletterSource). */
export interface NewsletterSource {
  feedId: number;
  title: string;
  host: string;
  port: number;
  username: string;
  folder: string;
}

/** Payload for add_newsletter_source (mirrors commands::NewsletterInput). */
export interface NewsletterInput {
  title: string | null;
  host: string;
  port: number;
  username: string;
  password: string;
  folder: string;
}

/** A feed-discovery result (mirrors discovery::DiscoveryResult). */
export interface DiscoveryResult {
  title: string;
  feedUrl: string;
  siteUrl: string | null;
  category: string | null;
  description: string | null;
  /** true → curated directory entry, false → live page scrape. */
  fromDirectory: boolean;
}

export interface Folder {
  id: number;
  name: string;
  position: number;
}

export interface Feed {
  id: number;
  feedUrl: string;
  siteUrl: string | null;
  title: string;
  description: string | null;
  faviconUrl: string | null;
  folderId: number | null;
  sourceType: SourceType;
  lastFetchedAt: string | null;
  fetchError: string | null;
  unreadCount: number;
  /** Per-feed refresh interval in minutes. `null` follows the global
   *  setting; the `525600` sentinel means "never". */
  refreshIntervalMin: number | null;
  /** When true, opening an article from this feed auto-translates it into the
   *  configured target language. Defaults to false (show the original). */
  autoTranslate: boolean;
  /** How articles from this feed open: reader view, auto-extracted full text,
   *  or the embedded web view of the original page. `null` follows the default
   *  behaviour (reader view, honouring the global auto-extract preference). */
  openMode: "reader" | "extracted" | "web" | null;
}

export interface Enclosure {
  url: string;
  mimeType: string | null;
  length: number | null;
}

/** Admin closed vocabulary vs free-form AI-generated tags. */
export type TagKind = "interest" | "ai";

export interface Tag {
  id: number;
  name: string;
  color: string;
  position: number;
  /** Defaults to interest when omitted by older payloads. */
  kind?: TagKind;
  articleCount: number;
}

export type RuleField = "title" | "author" | "content" | "any";
export type RuleAction = "skip" | "read" | "star";

/** Dry-run result for a draft filter rule (see preview_rule command). */
export interface RulePreview {
  count: number;
  samples: string[];
}

export interface Rule {
  id: number;
  name: string;
  enabled: boolean;
  feedId: number | null;
  field: RuleField;
  query: string;
  action: RuleAction;
  position: number;
}

export interface ArticlePreviewTranslation {
  articleId: number;
  title: string;
  snippet: string;
  lang: string;
  engine: string;
}

export interface ArticleSummary {
  id: number;
  feedId: number;
  feedTitle: string;
  sourceType: SourceType;
  title: string;
  author: string | null;
  snippet: string | null;
  imageUrl: string | null;
  url: string | null;
  publishedAt: string | null;
  isRead: boolean;
  isStarred: boolean;
  readLater: boolean;
}

export interface ArticleDetail {
  id: number;
  feedId: number;
  feedTitle: string;
  sourceType: SourceType;
  title: string;
  author: string | null;
  url: string | null;
  contentHtml: string | null;
  extractedHtml: string | null;
  imageUrl: string | null;
  publishedAt: string | null;
  isRead: boolean;
  isStarred: boolean;
  readLater: boolean;
  aiSummary: string | null;
  /** Cached translated body HTML, if a translation has been generated. */
  translatedHtml: string | null;
  /** The target language code the cached translation was produced for. */
  translatedLang: string | null;
  enclosures: Enclosure[];
  tags: Tag[];
}

export interface SmartCounts {
  unread: number;
  starred: number;
  readLater: number;
}

/** A user highlight / annotation (mirrors models::Highlight). */
export interface Highlight {
  id: number;
  articleId: number;
  quote: string;
  prefix: string;
  suffix: string;
  textOffset: number;
  color: string;
  note: string;
  createdAt: string;
}

// Mirrors the adjacently-tagged Rust `ArticleQuery` enum.
export type ArticleQuery =
  | { kind: "all" }
  | { kind: "unread" }
  | { kind: "starred" }
  | { kind: "readLater" }
  | { kind: "feed"; value: number }
  | { kind: "folder"; value: number }
  | { kind: "tag"; value: number };

export type AiEvent =
  | { type: "delta"; data: string }
  | { type: "done" }
  | { type: "error"; data: string };

/** Batch-level translation progress (mirrors commands::TranslateEvent). */
export type TranslateEvent =
  | { type: "start"; data: { total: number } }
  | { type: "batch"; data: { html: string; done: number } }
  | { type: "done"; data: { html: string } }
  | { type: "error"; data: string };

export type RefreshProgress =
  | { event: "started"; data: { total: number } }
  | {
      event: "feedDone";
      data: { feedId: number; newArticles: number; error: string | null };
    }
  | { event: "finished"; data: { newArticles: number } };
