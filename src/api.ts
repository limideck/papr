// HTTP API client for papr-server.
//
// Function names match the former Tauri command surface so existing React
// components keep working. Paths are under `/api/...` with cookie sessions.
// JSON uses camelCase (aligned with `types.ts`); a few admin endpoints also
// accept/emit snake_case for FO-shaped feed-source payloads — normalised here.

import { imageBytes, type ImageBytesResponse } from "./lib/imageBytes";
import { apiBytes, apiJson, apiStream, qs } from "./lib/http";
import type {
  AiEvent,
  ArticleDetail,
  ArticlePreviewTranslation,
  ArticleQuery,
  ArticleSummary,
  AutoTagStatus,
  DiscoveryResult,
  Feed,
  FeedSource,
  FeedSourceScanResult,
  Folder,
  Highlight,
  ManagedUser,
  NewsletterInput,
  NewsletterSource,
  RefreshProgress,
  Rule,
  RuleAction,
  RuleField,
  RulePreview,
  SessionUser,
  SmartCounts,
  StatsOverview,
  Tag,
  TagAlias,
  TagKind,
  TranslateEvent,
  WordCloudEntities,
  WordCloudEntitiesSource,
  WordCloudEntity,
  WordCloudResult,
  WordCloudStopwords,
} from "./types";

const API = "/api";

/** Backend often wraps scalar results as `{ id }` / `{ count }` / `{ value }`. */
function unwrapId(data: number | { id: number }): number {
  return typeof data === "number" ? data : data.id;
}
function unwrapCount(data: number | { count: number }): number {
  return typeof data === "number" ? data : data.count;
}
function unwrapSetting(data: string | null | { value: string | null }): string | null {
  if (data !== null && typeof data === "object" && "value" in data) {
    return data.value;
  }
  return data;
}

// ── auth ──
type RawSession = SessionUser & { is_admin?: boolean; user?: string };

function normalizeSession(u: RawSession): SessionUser {
  return {
    id: u.id,
    username: u.username || u.user || "",
    isAdmin: Boolean(u.isAdmin ?? u.is_admin),
  };
}

export const getMe = () =>
  apiJson<RawSession>(`${API}/auth/me`).then(normalizeSession);

export const login = (username: string, password: string) =>
  apiJson<RawSession>(`${API}/auth/login`, {
    method: "POST",
    body: JSON.stringify({ username, password }),
  }).then(normalizeSession);

export const logout = () =>
  apiJson<void>(`${API}/auth/logout`, { method: "POST" });

// ── users (admin) ──
type RawManagedUser = ManagedUser & {
  is_admin?: boolean;
  created_at?: string;
};

function normalizeManagedUser(u: RawManagedUser): ManagedUser {
  return {
    id: u.id,
    username: u.username,
    isAdmin: Boolean(u.isAdmin ?? u.is_admin),
    createdAt: u.createdAt ?? u.created_at,
  };
}

export const listUsers = () =>
  apiJson<RawManagedUser[] | { users: RawManagedUser[] }>(`${API}/users`).then(
    (data) => {
      const list = Array.isArray(data) ? data : (data.users ?? []);
      return list.map(normalizeManagedUser);
    },
  );

export const createUser = (
  username: string,
  password: string,
  isAdmin: boolean,
) =>
  apiJson<number | { id: number }>(`${API}/users`, {
    method: "POST",
    body: JSON.stringify({ username, password, isAdmin }),
  }).then(unwrapId);

export const deleteUser = (id: number) =>
  apiJson<void>(`${API}/users/${id}`, { method: "DELETE" });

export const patchUser = (
  id: number,
  patch: { isAdmin?: boolean; password?: string },
) =>
  apiJson<void>(`${API}/users/${id}`, {
    method: "PATCH",
    body: JSON.stringify(patch),
  });

export const changePassword = (oldPassword: string, newPassword: string) =>
  apiJson<void>(`${API}/users/me/password`, {
    method: "POST",
    body: JSON.stringify({ oldPassword, newPassword }),
  });

// ── auto-tag (admin) ──
type RawAutoTagStatus = AutoTagStatus & {
  last_error?: string | null;
  recent_errors?: { article_id?: number; error?: string; at?: string }[];
};

export const getAutoTagStatus = (days = 7) =>
  apiJson<RawAutoTagStatus>(
    `${API}/auto-tag/status${qs({ days })}`,
  ).then(normalizeAutoTagStatus);

function normalizeAutoTagStatus(raw: RawAutoTagStatus): AutoTagStatus {
  const recent = raw.recentErrors ?? raw.recent_errors;
  return {
    pending: raw.pending,
    processing: raw.processing,
    failed: raw.failed,
    done: raw.done,
    enabled: raw.enabled,
    lastError: raw.lastError ?? raw.last_error ?? null,
    windowDays: raw.windowDays,
    articlesInWindow: raw.articlesInWindow,
    untaggedInWindow: raw.untaggedInWindow,
    taggedInWindow: raw.taggedInWindow,
    recentErrors: recent?.map((e) => {
      const row = e as {
        articleId?: number;
        article_id?: number;
        error?: string;
        at?: string;
      };
      return {
        articleId: row.articleId ?? row.article_id,
        error: row.error,
        at: row.at,
      };
    }),
  };
}

export const backfillAutoTag = (days: number, force = false) =>
  apiJson<{
    enqueued?: number;
    queued?: number;
    count?: number;
    days?: number;
    force?: boolean;
  }>(`${API}/auto-tag/backfill`, {
    method: "POST",
    body: JSON.stringify({ days, force }),
  }).then((res) => ({
    ...res,
    // Backend returns `enqueued`; keep aliases for toast/UI.
    enqueued: res.enqueued ?? res.queued ?? res.count,
  }));

/** Soft pause: drop pending/processing/failed; keep done. No auto-backfill. */
export const clearAutoTagQueue = () =>
  apiJson<{ cleared: number }>(`${API}/auto-tag/clear-queue`, {
    method: "POST",
  });

/** Admin overview: totals, tagged coverage, queue, daily ingest. */
export const getStatsOverview = (days = 30) =>
  apiJson<StatsOverview>(
    `${API}/stats/overview?days=${encodeURIComponent(String(days))}`,
  );

// ── folders ──
export const listFolders = () => apiJson<Folder[]>(`${API}/folders`);
export const createFolder = (name: string) =>
  apiJson<number | { id: number }>(`${API}/folders`, {
    method: "POST",
    body: JSON.stringify({ name }),
  }).then(unwrapId);
export const renameFolder = (id: number, name: string) =>
  apiJson<void>(`${API}/folders/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ name }),
  });
export const deleteFolder = (id: number) =>
  apiJson<void>(`${API}/folders/${id}`, { method: "DELETE" });

// ── images ──
/** Fetch image bytes through the server (Referer fallbacks for hotlink hosts). */
export const fetchImage = (url: string, pageUrl?: string | null) =>
  apiBytes(
    `${API}/fetch-image${qs({ url, pageUrl: pageUrl ?? undefined })}`,
  ).then((buf) => imageBytes(buf as ImageBytesResponse));

// ── feeds ──
export const listFeeds = () => apiJson<Feed[]>(`${API}/feeds`);
export const addFeed = (url: string, folderId: number | null) =>
  apiJson<Feed>(`${API}/feeds`, {
    method: "POST",
    body: JSON.stringify({ url, folderId }),
  });
export const searchFeedDirectory = (query: string, lang: string) =>
  apiJson<DiscoveryResult[]>(
    `${API}/feeds/discover${qs({ query, lang })}`,
  );
export const deleteFeed = (id: number) =>
  apiJson<void>(`${API}/feeds/${id}`, { method: "DELETE" });
export const moveFeed = (id: number, folderId: number | null) =>
  apiJson<void>(`${API}/feeds/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ folderId }),
  });
export const renameFeed = (id: number, title: string) =>
  apiJson<void>(`${API}/feeds/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ title }),
  });
export const setFeedRefreshInterval = (id: number, minutes: number | null) =>
  apiJson<void>(`${API}/feeds/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ refreshIntervalMin: minutes }),
  });
export const setFeedAutoTranslate = (id: number, enabled: boolean) =>
  apiJson<void>(`${API}/feeds/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ autoTranslate: enabled }),
  });
export const setFeedOpenMode = (
  id: number,
  mode: "reader" | "extracted" | "web" | null,
) =>
  apiJson<void>(`${API}/feeds/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ openMode: mode }),
  });

/** Refresh feeds. Returns new-article count. Optional `onProgress` receives a
 *  synthesised finished event today; the server may later stream SSE progress. */
export async function refreshFeeds(
  onProgress?: (p: RefreshProgress) => void,
  scope?: { feedId?: number; folderId?: number },
): Promise<number> {
  const data = await apiJson<{ newArticles?: number; count?: number }>(
    `${API}/feeds/refresh`,
    {
      method: "POST",
      body: JSON.stringify({
        feedId: scope?.feedId ?? null,
        folderId: scope?.folderId ?? null,
      }),
    },
  );
  const n = data.newArticles ?? data.count ?? 0;
  onProgress?.({ event: "finished", data: { newArticles: n } });
  return n;
}

// ── articles ──
function articleListParams(
  query: ArticleQuery,
  unreadOnly: boolean,
  search: string | null,
  oldestFirst: boolean,
  limit: number,
  offset: number,
  extra?: Record<string, string | number | boolean | null | undefined>,
) {
  return qs({
    kind: query.kind,
    value: "value" in query ? query.value : undefined,
    unreadOnly,
    search: search || undefined,
    oldestFirst,
    limit,
    offset,
    ...extra,
  });
}

export const listArticles = (
  query: ArticleQuery,
  unreadOnly: boolean,
  search: string | null,
  oldestFirst: boolean,
  limit: number,
  offset: number,
  sortByRelevance = true,
) =>
  apiJson<ArticleSummary[]>(
    `${API}/articles${articleListParams(query, unreadOnly, search, oldestFirst, limit, offset, {
      sortByRelevance: search ? sortByRelevance : undefined,
    })}`,
  );

export const articleIndex = (
  query: ArticleQuery,
  unreadOnly: boolean,
  oldestFirst: boolean,
  articleId: number,
) =>
  apiJson<number | null>(`${API}/articles/index`, {
    method: "POST",
    body: JSON.stringify({ query, unreadOnly, oldestFirst, articleId }),
  });

export const getArticle = (id: number) =>
  apiJson<ArticleDetail>(`${API}/articles/${id}`);
export const markRead = (id: number, read: boolean) =>
  apiJson<void>(`${API}/articles/${id}/read`, {
    method: "PUT",
    body: JSON.stringify({ value: read }),
  });
export const markStarred = (id: number, starred: boolean) =>
  apiJson<void>(`${API}/articles/${id}/starred`, {
    method: "PUT",
    body: JSON.stringify({ value: starred }),
  });
export const markReadLater = (id: number, value: boolean) =>
  apiJson<void>(`${API}/articles/${id}/read-later`, {
    method: "PUT",
    body: JSON.stringify({ value }),
  });
export const markAllRead = (query: ArticleQuery) =>
  apiJson<number | { count: number }>(`${API}/articles/mark-all-read`, {
    method: "POST",
    body: JSON.stringify({ query }),
  }).then(unwrapCount);
export const smartCounts = () =>
  apiJson<SmartCounts>(`${API}/smart-counts`);

// ── full-text extraction ──
export const extractFulltext = (articleId: number) =>
  apiJson<string | { html: string }>(
    `${API}/articles/${articleId}/extract`,
    { method: "POST" },
  ).then((data) => (typeof data === "string" ? data : data.html));

// ── OPML ──
export const importOpml = (content: string) =>
  apiJson<number | { count: number }>(`${API}/opml/import`, {
    method: "POST",
    body: JSON.stringify({ content }),
  }).then(unwrapCount);
export const exportOpml = () =>
  apiJson<string | { content: string }>(`${API}/opml/export`).then((data) =>
    typeof data === "string" ? data : data.content,
  );

// ── AI (SSE streaming) ──
export function aiSummarize(
  articleId: number,
  onToken: (e: AiEvent) => void,
): Promise<void> {
  return apiStream(
    `${API}/ai/summarize`,
    {
      method: "POST",
      body: JSON.stringify({ articleId }),
      headers: { Accept: "text/event-stream" },
    },
    (raw) => onToken(raw as AiEvent),
  );
}

export function aiAsk(
  question: string,
  onToken: (e: AiEvent) => void,
): Promise<void> {
  return apiStream(
    `${API}/ai/ask`,
    {
      method: "POST",
      body: JSON.stringify({ question }),
      headers: { Accept: "text/event-stream" },
    },
    (raw) => onToken(raw as AiEvent),
  );
}

export function aiDigest(onToken: (e: AiEvent) => void): Promise<void> {
  return apiStream(
    `${API}/ai/digest`,
    {
      method: "POST",
      body: JSON.stringify({}),
      headers: { Accept: "text/event-stream" },
    },
    (raw) => onToken(raw as AiEvent),
  );
}

export function aiTranslate(
  articleId: number,
  lang: string,
  engine: string,
  onEvent: (e: TranslateEvent) => void,
): Promise<void> {
  return apiStream(
    `${API}/ai/translate`,
    {
      method: "POST",
      body: JSON.stringify({ articleId, lang, engine }),
      headers: { Accept: "text/event-stream" },
    },
    (raw) => onEvent(raw as TranslateEvent),
  );
}

export const translateArticlePreview = (
  articleId: number,
  lang: string,
  engine: string,
) =>
  apiJson<ArticlePreviewTranslation>(
    `${API}/articles/${articleId}/translate-preview`,
    {
      method: "POST",
      body: JSON.stringify({ lang, engine }),
    },
  );

// ── settings ──
export const getSetting = (key: string) =>
  apiJson<string | null | { value: string | null }>(
    `${API}/settings/${encodeURIComponent(key)}`,
  ).then(unwrapSetting);
export const setSetting = (key: string, value: string) =>
  apiJson<void>(`${API}/settings/${encodeURIComponent(key)}`, {
    method: "PUT",
    body: JSON.stringify({ value }),
  });

// ── storage ──
export interface StorageStats {
  dbBytes: number;
  articleCount: number;
  feedCount: number;
}
export const storageStats = () =>
  apiJson<StorageStats>(`${API}/storage/stats`);

// ── ai usage ──
export interface AiUsageRow {
  feature: string;
  calls: number;
  promptTokens: number;
  completionTokens: number;
  reasoningTokens: number;
  cacheHitTokens: number;
}
export interface AiUsageReport {
  total: AiUsageRow;
  byFeature: AiUsageRow[];
  estimatedCost: number;
}
export const aiUsage = (days: number = 30) =>
  apiJson<AiUsageReport>(`${API}/ai/usage${qs({ days })}`);

export interface BalanceDay {
  day: string;
  totalBalance: number;
  grantedBalance: number;
  toppedUpBalance: number;
  spend: number | null;
  topup: number | null;
}
export interface OfficialUsageDay {
  day: string;
  tokens: number;
  cost: number;
}
export interface BalanceReport {
  latest: BalanceDay | null;
  history: BalanceDay[];
  officialUsage: OfficialUsageDay[];
}
export const aiBalance = (days: number = 30) =>
  apiJson<BalanceReport>(`${API}/ai/balance${qs({ days })}`);
export const aiBalanceRefresh = () =>
  apiJson<{ ok: boolean }>(`${API}/ai/balance/refresh`, { method: "POST" });
export const cleanupArticles = (days: number) =>
  apiJson<number | { count: number }>(`${API}/storage/cleanup`, {
    method: "POST",
    body: JSON.stringify({ days }),
  }).then(unwrapCount);
export const vacuumDb = () =>
  apiJson<void>(`${API}/storage/vacuum`, { method: "POST" });
export const resetSettings = () =>
  apiJson<void>(`${API}/storage/reset-settings`, { method: "POST" });
export const clearAllData = () =>
  apiJson<void>(`${API}/storage/clear`, { method: "POST" });

// ── network ──
export const applyNetworkSettings = () =>
  apiJson<void>(`${API}/network/apply`, { method: "POST" });

// ── GReader sync (FreshRSS / Miniflux) ──
export type GReaderProvider = "freshrss" | "miniflux";
export interface FreshRssStatus {
  connected: boolean;
  url: string | null;
  provider: GReaderProvider;
}
export const freshrssStatus = () =>
  apiJson<FreshRssStatus>(`${API}/sync/status`);
export const freshrssConnect = (
  url: string,
  username: string,
  password: string,
  provider: GReaderProvider = "freshrss",
) =>
  apiJson<void>(`${API}/sync/connect`, {
    method: "POST",
    body: JSON.stringify({ url, username, password, provider }),
  });
export const freshrssDisconnect = () =>
  apiJson<void>(`${API}/sync/disconnect`, { method: "POST" });
export const freshrssSync = () =>
  apiJson<number>(`${API}/sync/sync`, { method: "POST" });

// ── tray (desktop-only; no-op on web) ──
export const refreshTray = () => Promise.resolve();

// ── deep links (desktop-only; no-op on web) ──
export const takePendingDeepLink = () => Promise.resolve<string | null>(null);

// ── tags ──
export const listTags = (kind?: TagKind) =>
  apiJson<Tag[]>(`${API}/tags${kind ? qs({ kind }) : ""}`);
export const createTag = (name: string, kind: TagKind = "interest") =>
  apiJson<number | { id: number }>(`${API}/tags`, {
    method: "POST",
    body: JSON.stringify({ name, kind }),
  }).then(unwrapId);
export const renameTag = (id: number, name: string) =>
  apiJson<void>(`${API}/tags/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ name }),
  });
export const setTagColor = (id: number, color: string) =>
  apiJson<void>(`${API}/tags/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ color }),
  });
export const deleteTag = (id: number) =>
  apiJson<void>(`${API}/tags/${id}`, { method: "DELETE" });
/** Delete AI tags with zero articles. Interest tags are never cleaned up. */
export const cleanupEmptyTags = (kind: "ai" = "ai") =>
  apiJson<{ deleted: number }>(`${API}/tags/cleanup-empty`, {
    method: "POST",
    body: JSON.stringify({ kind }),
  });
export const reorderTags = (ids: number[]) =>
  apiJson<void>(`${API}/tags/reorder`, {
    method: "POST",
    body: JSON.stringify({ ids }),
  });
export const setArticleTag = (articleId: number, tagId: number, on: boolean) =>
  apiJson<void>(`${API}/articles/${articleId}/tags/${tagId}`, {
    method: "PUT",
    body: JSON.stringify({ on }),
  });

export const listTagAliases = (opts?: { tagId?: number; kind?: TagKind }) => {
  const params: Record<string, string | number> = {};
  if (opts?.tagId != null) params.tag_id = opts.tagId;
  if (opts?.kind) params.kind = opts.kind;
  return apiJson<TagAlias[]>(`${API}/tags/aliases${qs(params)}`);
};
export const createTagAlias = (tagId: number, alias: string) =>
  apiJson<number | { id: number }>(`${API}/tags/aliases`, {
    method: "POST",
    body: JSON.stringify({ tagId, alias }),
  }).then(unwrapId);
export const renameTagAlias = (id: number, alias: string) =>
  apiJson<void>(`${API}/tags/aliases/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ alias }),
  });
export const deleteTagAlias = (id: number) =>
  apiJson<void>(`${API}/tags/aliases/${id}`, { method: "DELETE" });

/** Run interest + AI auto-tag on one article (sync). Returns updated tags. */
export const autoTagArticle = (articleId: number) =>
  apiJson<{ ok: boolean; tags: Tag[] }>(
    `${API}/articles/${articleId}/auto-tag`,
    { method: "POST" },
  );

// ── filter rules ──
export const listRules = () => apiJson<Rule[]>(`${API}/rules`);
export const createRule = (
  name: string,
  feedId: number | null,
  field: RuleField,
  query: string,
  action: RuleAction,
) =>
  apiJson<number | { id: number }>(`${API}/rules`, {
    method: "POST",
    body: JSON.stringify({ name, feedId, field, query, action }),
  }).then(unwrapId);
export const updateRule = (
  id: number,
  name: string,
  enabled: boolean,
  feedId: number | null,
  field: RuleField,
  query: string,
  action: RuleAction,
) =>
  apiJson<void>(`${API}/rules/${id}`, {
    method: "PUT",
    body: JSON.stringify({ name, enabled, feedId, field, query, action }),
  });
export const deleteRule = (id: number) =>
  apiJson<void>(`${API}/rules/${id}`, { method: "DELETE" });
export const previewRule = (
  feedId: number | null,
  field: RuleField,
  query: string,
) =>
  apiJson<RulePreview>(`${API}/rules/preview`, {
    method: "POST",
    body: JSON.stringify({ feedId, field, query }),
  });
export const applyRuleToExisting = (
  feedId: number | null,
  field: RuleField,
  query: string,
  action: RuleAction,
) =>
  apiJson<number | { count: number }>(`${API}/rules/apply`, {
    method: "POST",
    body: JSON.stringify({ feedId, field, query, action }),
  }).then(unwrapCount);

// ── highlights ──
export interface NewHighlight {
  articleId: number;
  quote: string;
  prefix: string;
  suffix: string;
  textOffset: number;
  color: string;
  note: string;
}
export const createHighlight = (h: NewHighlight) =>
  apiJson<number>(`${API}/highlights`, {
    method: "POST",
    body: JSON.stringify(h),
  });
export const listHighlights = (articleId: number) =>
  apiJson<Highlight[]>(`${API}/highlights${qs({ articleId })}`);
export const listAllHighlights = () =>
  apiJson<Highlight[]>(`${API}/highlights`);
export const updateHighlightNote = (id: number, note: string) =>
  apiJson<void>(`${API}/highlights/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ note }),
  });
export const setHighlightColor = (id: number, color: string) =>
  apiJson<void>(`${API}/highlights/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ color }),
  });
export const deleteHighlight = (id: number) =>
  apiJson<void>(`${API}/highlights/${id}`, { method: "DELETE" });

// ── newsletter sources ──
export const addNewsletterSource = (input: NewsletterInput) =>
  apiJson<Feed>(`${API}/newsletters`, {
    method: "POST",
    body: JSON.stringify(input),
  });
export const listNewsletterSources = () =>
  apiJson<NewsletterSource[]>(`${API}/newsletters`);
export const removeNewsletterSource = (feedId: number) =>
  apiJson<void>(`${API}/newsletters/${feedId}`, { method: "DELETE" });

// ── original-page view (web: open in a new tab) ──
export interface PageViewBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}
export const openPageView = (url: string, _b: PageViewBounds) => {
  window.open(url, "_blank", "noopener,noreferrer");
  return Promise.resolve();
};
export const setPageViewBounds = (_b: PageViewBounds) => Promise.resolve();
export const setPageViewVisible = (_visible: boolean) => Promise.resolve();
export const closePageView = () => Promise.resolve();

// ── word cloud ──
export async function getWordCloud(params: {
  days?: number;
  from?: string;
  to?: string;
  refresh?: boolean;
}): Promise<WordCloudResult> {
  const raw = await apiJson<WordCloudResult>(
    `${API}/wordcloud${qs({
      days: params.days,
      from: params.from,
      to: params.to,
      refresh: params.refresh ? 1 : undefined,
    })}`,
  );
  const terms = raw.terms ?? [];
  return {
    terms,
    // Real article rows scanned in-range; never fall back to terms.length
    // (that is the top-N term cap, typically ~100).
    scanned: typeof raw.scanned === "number" ? raw.scanned : 0,
    from: raw.from,
    to: raw.to,
  };
}

export async function getWordCloudStopwords(): Promise<WordCloudStopwords> {
  return apiJson(`${API}/wordcloud/stopwords`);
}

export async function getWordCloudEntities(): Promise<WordCloudEntities> {
  return apiJson(`${API}/wordcloud/entities`);
}

export type PatchWordCloudEntityBody = {
  canonical?: string;
  aliases?: string[];
};

export type PatchWordCloudEntityResult = {
  ok: boolean;
  entity: WordCloudEntity;
  version: number;
  source?: WordCloudEntitiesSource;
  path?: string;
  writable?: boolean;
  seedDir?: string;
  cowDir?: string;
  dictVersion?: number;
  dictBumped?: boolean;
};

/** Admin: update entity display canonical (and optional aliases). Triggers COW on first edit. */
export async function patchWordCloudEntity(
  id: string,
  body: PatchWordCloudEntityBody,
): Promise<PatchWordCloudEntityResult> {
  const raw = await apiJson<Record<string, unknown>>(
    `${API}/wordcloud/entities/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      body: JSON.stringify(body),
    },
  );
  return {
    ok: Boolean(raw.ok),
    entity: raw.entity as WordCloudEntity,
    version: Number(raw.version ?? 1),
    source: raw.source as WordCloudEntitiesSource | undefined,
    path: typeof raw.path === "string" ? raw.path : undefined,
    writable: typeof raw.writable === "boolean" ? raw.writable : undefined,
    seedDir: typeof raw.seedDir === "string" ? raw.seedDir : undefined,
    cowDir: typeof raw.cowDir === "string" ? raw.cowDir : undefined,
    dictVersion:
      typeof raw.dictVersion === "number" ? raw.dictVersion : undefined,
    dictBumped:
      typeof raw.dictBumped === "boolean" ? raw.dictBumped : undefined,
  };
}

export type CreateWordCloudEntityBody = {
  id?: string;
  canonical: string;
  group?: string;
  aliases?: string[];
};

/** Admin: create / promote a residual cloud term to an entity (COW on first write). */
export async function createWordCloudEntity(
  body: CreateWordCloudEntityBody,
): Promise<PatchWordCloudEntityResult> {
  const raw = await apiJson<Record<string, unknown>>(
    `${API}/wordcloud/entities`,
    {
      method: "POST",
      body: JSON.stringify(body),
    },
  );
  return {
    ok: Boolean(raw.ok),
    entity: raw.entity as WordCloudEntity,
    version: Number(raw.version ?? 1),
    source: raw.source as WordCloudEntitiesSource | undefined,
    path: typeof raw.path === "string" ? raw.path : undefined,
    writable: typeof raw.writable === "boolean" ? raw.writable : undefined,
    seedDir: typeof raw.seedDir === "string" ? raw.seedDir : undefined,
    cowDir: typeof raw.cowDir === "string" ? raw.cowDir : undefined,
    dictVersion:
      typeof raw.dictVersion === "number" ? raw.dictVersion : undefined,
    dictBumped:
      typeof raw.dictBumped === "boolean" ? raw.dictBumped : undefined,
  };
}

export type WordCloudIndexStatus = {
  dictVersion: number;
  indexed: number;
  stale: number;
  missing: number;
  totalArticles: number;
};

export async function getWordCloudStatus(): Promise<WordCloudIndexStatus> {
  const raw = await apiJson<Record<string, number>>(`${API}/wordcloud/status`);
  return {
    dictVersion: raw.dictVersion ?? raw.dict_version ?? 0,
    indexed: raw.indexed ?? 0,
    stale: raw.stale ?? 0,
    missing: raw.missing ?? 0,
    totalArticles: raw.totalArticles ?? raw.total_articles ?? 0,
  };
}

/** Run one sync backfill batch (or just report status when sync=false). */
export async function backfillWordCloud(opts?: {
  sync?: boolean;
  limit?: number;
}): Promise<
  WordCloudIndexStatus & {
    ok: boolean;
    sync: boolean;
    processed?: number;
    remaining?: number;
  }
> {
  const raw = await apiJson<Record<string, unknown>>(`${API}/wordcloud/backfill`, {
    method: "POST",
    body: JSON.stringify({
      sync: opts?.sync ?? true,
      limit: opts?.limit,
    }),
  });
  return {
    ok: Boolean(raw.ok),
    sync: Boolean(raw.sync),
    processed: typeof raw.processed === "number" ? raw.processed : undefined,
    remaining: typeof raw.remaining === "number" ? raw.remaining : undefined,
    dictVersion: Number(raw.dictVersion ?? raw.dict_version ?? 0),
    indexed: Number(raw.indexed ?? 0),
    stale: Number(raw.stale ?? 0),
    missing: Number(raw.missing ?? 0),
    totalArticles: Number(raw.totalArticles ?? raw.total_articles ?? 0),
  };
}

// ── admin: directory index feed sources (`/api/feed-sources`) ──
type RawFeedSource = {
  id: number;
  baseUrl?: string;
  base_url?: string;
  lastCheckedAt?: string | null;
  last_checked_at?: number | string | null;
  feedCount?: number;
  feed_count?: number;
  folderId?: number | null;
  folder_id?: number | null;
  folderName?: string | null;
  folder_name?: string | null;
};

function normalizeFeedSource(raw: RawFeedSource): FeedSource {
  let lastCheckedAt: string | null = null;
  const rawChecked = raw.lastCheckedAt ?? raw.last_checked_at;
  if (typeof rawChecked === "string") lastCheckedAt = rawChecked;
  else if (typeof rawChecked === "number") {
    lastCheckedAt = new Date(rawChecked * (rawChecked < 1e12 ? 1000 : 1)).toISOString();
  }
  return {
    id: raw.id,
    baseUrl: raw.baseUrl ?? raw.base_url ?? "",
    lastCheckedAt,
    feedCount: raw.feedCount ?? raw.feed_count ?? 0,
    folderId: raw.folderId ?? raw.folder_id ?? null,
    folderName: raw.folderName ?? raw.folder_name ?? null,
  };
}

type RawScan = {
  addedCount?: number;
  added?: { url: string; name?: string; id?: number }[];
  skipped?: number;
  stale?: { id: string | number; name: string; url: string; reason?: string }[];
};

function normalizeScanResult(raw: unknown): FeedSourceScanResult {
  const items: RawScan[] = Array.isArray(raw)
    ? (raw as RawScan[])
    : raw && typeof raw === "object"
      ? [raw as RawScan]
      : [];
  let addedCount = 0;
  let skipped = 0;
  const added: NonNullable<FeedSourceScanResult["added"]> = [];
  const stale: NonNullable<FeedSourceScanResult["stale"]> = [];
  for (const item of items) {
    const list = item.added ?? [];
    addedCount += item.addedCount ?? list.length;
    skipped += item.skipped ?? 0;
    for (const a of list) added.push({ url: a.url, name: a.name });
    for (const s of item.stale ?? []) stale.push(s);
  }
  return { addedCount, skipped, added, stale };
}

export const listFeedSources = async () => {
  const data = await apiJson<RawFeedSource[] | { sources: RawFeedSource[] }>(
    `${API}/feed-sources`,
  );
  const list = Array.isArray(data) ? data : (data.sources ?? []);
  return list.map(normalizeFeedSource);
};

export const addFeedSource = async (baseUrl: string) => {
  const raw = await apiJson<RawFeedSource | { id: number }>(
    `${API}/feed-sources`,
    {
      method: "POST",
      body: JSON.stringify({ baseUrl }),
    },
  );
  return normalizeFeedSource(raw as RawFeedSource);
};

export const removeFeedSource = (id: number) =>
  apiJson<void>(`${API}/feed-sources/${id}`, { method: "DELETE" });

export const scanFeedSources = async (id?: number) => {
  const path =
    id != null
      ? `${API}/feed-sources/${id}/scan`
      : `${API}/feed-sources/scan`;
  const raw = await apiJson<unknown>(path, { method: "POST" });
  return normalizeScanResult(raw);
};
