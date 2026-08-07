import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useAuth } from "../auth";
import { useUi } from "../store";
import { useArticleActions } from "../hooks/articleActions";
import { withUndo, reportError } from "../toast";
import { tagColor, TAG_PALETTE } from "../lib/tagColors";
import type { ArticleQuery, Feed, Folder, Tag } from "../types";
import Icon, { type IconName } from "./Icon";
import ContextMenu, { type MenuEntry } from "./ContextMenu";
import FeedAvatar from "./FeedAvatar";
import PromptDialog from "./PromptDialog";
import WordCloudPanel from "./WordCloudPanel";

interface Props {
  onAddFeed: () => void;
  /** Opens the Add-feed dialog on its Explore tab. */
  onExplore: () => void;
  onOpenSettings: (section?: string) => void;
  /** Refresh feeds. With no scope refreshes everything (the toolbar button);
   *  pass `{ feedId }` or `{ folderId }` for the per-source context menus. */
  onRefresh: (scope?: { feedId?: number; folderId?: number }) => void;
  refreshing: boolean;
  onToast: (msg: string) => void;
}

const sameQuery = (a: ArticleQuery, b: ArticleQuery) =>
  JSON.stringify(a) === JSON.stringify(b);

/** Enter / Space activator for a div that behaves as a button — gives the
 *  sidebar's clickable rows keyboard parity with their onClick. */
const onActivate = (fn: () => void) => (e: React.KeyboardEvent) => {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    fn();
  }
};

type Menu =
  | { x: number; y: number; kind: "feed"; feed: Feed }
  | { x: number; y: number; kind: "folder"; folder: Folder }
  | { x: number; y: number; kind: "tag"; tag: Tag };

type Prompt = {
  title: string;
  initial: string;
  placeholder: string;
  onSubmit: (v: string) => void;
};

function SbItem({
  icon,
  label,
  count,
  active,
  onClick,
}: {
  icon: IconName;
  label: string;
  count?: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <div
      className={`sb-item ${active ? "active" : ""}`}
      role="button"
      tabIndex={0}
      aria-current={active || undefined}
      onClick={onClick}
      onKeyDown={onActivate(onClick)}
    >
      <span className="sb-ico">
        <Icon name={icon} size={15} />
      </span>
      <span className="sb-label">{label}</span>
      {count != null && count > 0 && <span className="sb-count">{count}</span>}
    </div>
  );
}

export default function Sidebar({
  onAddFeed,
  onExplore,
  onOpenSettings,
  onRefresh,
  refreshing,
  onToast,
}: Props) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const isAdmin = !!user?.isAdmin;
  const qc = useQueryClient();
  const actions = useArticleActions();
  const query = useUi((s) => s.query);
  const select = useUi((s) => s.select);
  const listSearch = useUi((s) => s.listSearch);
  const setListSearch = useUi((s) => s.setListSearch);
  const showCounts = useUi((s) => s.prefs.showSidebarCounts);
  const unreadOnly = useUi((s) => s.prefs.sidebarUnreadOnly);
  const setPref = useUi((s) => s.setPref);
  const [sideTab, setSideTab] = useState<"feeds" | "cloud" | "tags">("feeds");
  // Draft until Enter — keeps typing independent of listSearch, but stays in
  // sync when the list filter changes elsewhere (word cloud / clear chips).
  const [searchDraft, setSearchDraft] = useState(listSearch ?? "");
  useEffect(() => {
    setSearchDraft(listSearch ?? "");
  }, [listSearch]);

  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  const folders = useQuery({ queryKey: ["folders"], queryFn: api.listFolders });
  const counts = useQuery({ queryKey: ["counts"], queryFn: api.smartCounts });
  const tags = useQuery({ queryKey: ["tags"], queryFn: () => api.listTags() });

  const [collapsed, setCollapsed] = useState<Record<number, boolean>>(() => {
    try {
      return JSON.parse(localStorage.getItem("collapsedFolders") || "{}");
    } catch {
      return {};
    }
  });
  useEffect(() => {
    localStorage.setItem("collapsedFolders", JSON.stringify(collapsed));
  }, [collapsed]);

  // Feed list sort: alpha by title, or by unread count. Persisted so the
  // sidebar keeps the user's preferred triage order across restarts.
  type FeedSortMode = "alpha" | "unread";
  type FeedSortDir = "asc" | "desc";
  const FEED_SORT_KEY = "papr.feedSort";
  const defaultDir = (mode: FeedSortMode): FeedSortDir =>
    mode === "unread" ? "desc" : "asc";
  const [feedSort, setFeedSort] = useState<{
    mode: FeedSortMode;
    dir: FeedSortDir;
  }>(() => {
    try {
      const raw = localStorage.getItem(FEED_SORT_KEY);
      if (!raw) return { mode: "unread", dir: "desc" };
      // Accept "alpha" / "unread" or "alpha:asc" / "unread:desc".
      const [modePart, dirPart] = raw.split(":");
      const mode: FeedSortMode =
        modePart === "alpha" || modePart === "unread" ? modePart : "unread";
      const dir: FeedSortDir =
        dirPart === "asc" || dirPart === "desc"
          ? dirPart
          : defaultDir(mode);
      return { mode, dir };
    } catch {
      return { mode: "unread", dir: "desc" };
    }
  });
  useEffect(() => {
    localStorage.setItem(
      FEED_SORT_KEY,
      `${feedSort.mode}:${feedSort.dir}`,
    );
  }, [feedSort]);

  const setSortMode = (mode: FeedSortMode) => {
    setFeedSort((prev) =>
      prev.mode === mode
        ? { mode, dir: prev.dir === "asc" ? "desc" : "asc" }
        : { mode, dir: defaultDir(mode) },
    );
  };

  const sortFeeds = (list: Feed[]): Feed[] => {
    const sorted = [...list];
    const mul = feedSort.dir === "asc" ? 1 : -1;
    if (feedSort.mode === "alpha") {
      sorted.sort(
        (a, b) =>
          mul *
          a.title.localeCompare(b.title, undefined, { sensitivity: "base" }),
      );
    } else {
      sorted.sort((a, b) => {
        const byUnread = (a.unreadCount - b.unreadCount) * mul;
        if (byUnread !== 0) return byUnread;
        return a.title.localeCompare(b.title, undefined, {
          sensitivity: "base",
        });
      });
    }
    return sorted;
  };

  // Tag list sort: alpha by name, or by article count. Same persistence
  // pattern as feed sort so the Tags tab keeps the preferred order.
  type TagSortMode = "alpha" | "count";
  type TagSortDir = "asc" | "desc";
  const TAG_SORT_KEY = "papr.tagSort";
  const defaultTagDir = (mode: TagSortMode): TagSortDir =>
    mode === "count" ? "desc" : "asc";
  const [tagSort, setTagSort] = useState<{
    mode: TagSortMode;
    dir: TagSortDir;
  }>(() => {
    try {
      const raw = localStorage.getItem(TAG_SORT_KEY);
      if (!raw) return { mode: "count", dir: "desc" };
      const [modePart, dirPart] = raw.split(":");
      const mode: TagSortMode =
        modePart === "alpha" || modePart === "count" ? modePart : "count";
      const dir: TagSortDir =
        dirPart === "asc" || dirPart === "desc"
          ? dirPart
          : defaultTagDir(mode);
      return { mode, dir };
    } catch {
      return { mode: "count", dir: "desc" };
    }
  });
  useEffect(() => {
    localStorage.setItem(TAG_SORT_KEY, `${tagSort.mode}:${tagSort.dir}`);
  }, [tagSort]);

  const setTagSortMode = (mode: TagSortMode) => {
    setTagSort((prev) =>
      prev.mode === mode
        ? { mode, dir: prev.dir === "asc" ? "desc" : "asc" }
        : { mode, dir: defaultTagDir(mode) },
    );
  };

  const sortTags = (list: Tag[]): Tag[] => {
    const sorted = [...list];
    const mul = tagSort.dir === "asc" ? 1 : -1;
    if (tagSort.mode === "alpha") {
      sorted.sort(
        (a, b) =>
          mul *
          a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
      );
    } else {
      sorted.sort((a, b) => {
        const byCount = (a.articleCount - b.articleCount) * mul;
        if (byCount !== 0) return byCount;
        return a.name.localeCompare(b.name, undefined, {
          sensitivity: "base",
        });
      });
    }
    return sorted;
  };

  const [menu, setMenu] = useState<Menu | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [dragId, setDragId] = useState<number | null>(null);
  const [dropFolder, setDropFolder] = useState<number | "none" | null>(null);
  const [tagDragId, setTagDragId] = useState<number | null>(null);
  const [tagOverId, setTagOverId] = useState<number | null>(null);

  // Feed/folder/tag mutations only touch the article-bearing caches — a bare
  // invalidateQueries() would also refetch AI summaries, settings and storage
  // stats. refreshAfterBulk() invalidates just the relevant keys.
  const guard = (p: Promise<unknown>, ok: string) =>
    p
      .then(() => {
        actions.refreshAfterBulk();
        onToast(ok);
      })
      .catch((e) => reportError(e));

  // Destructive feed/folder/tag deletes run behind an Undo window: the row
  // disappears at once, but the irreversible backend call is deferred ~6s so
  // a misclick can be taken back. `makeDelete` is a thunk — the backend call
  // must not start until the window actually closes.
  //
  // The optimistic removal also resets a now-dangling selection: the active
  // view, or the open article, may point at the entity that just vanished.
  // Deleting a feed cascade-deletes its articles, so a still-open article
  // from it is closed too (the reader would otherwise show a load error).
  const guardDelete = (
    makeDelete: () => Promise<unknown>,
    toastText: string,
    kind: "feed" | "folder" | "tag",
    id: number,
  ) => {
    const cacheKey =
      kind === "feed" ? "feeds" : kind === "folder" ? "folders" : "tags";
    // Snapshots taken before the optimistic edit, restored verbatim on undo
    // (or if the eventual delete fails).
    const prevList = qc.getQueryData<{ id: number }[]>([cacheKey]);
    const prevFeeds =
      kind === "folder" ? qc.getQueryData<Feed[]>(["feeds"]) : undefined;
    const restore = () => {
      if (prevList) qc.setQueryData([cacheKey], prevList);
      if (prevFeeds) qc.setQueryData(["feeds"], prevFeeds);
    };

    withUndo({
      text: toastText,
      apply: () => {
        qc.setQueryData<{ id: number }[]>([cacheKey], (old) =>
          old?.filter((x) => x.id !== id),
        );
        // Deleting a folder orphans its feeds to "uncategorized" (the DB FK
        // is ON DELETE SET NULL) — mirror that so they don't briefly vanish
        // from the tree during the undo window.
        if (kind === "folder") {
          qc.setQueryData<Feed[]>(["feeds"], (old) =>
            old?.map((f) =>
              f.folderId === id ? { ...f, folderId: null } : f,
            ),
          );
        }
        const st = useUi.getState();
        if (st.query.kind === kind && st.query.value === id) {
          st.select({ kind: "all" }, t("smart.all"));
        } else if (kind === "feed" && st.selectedArticleId != null) {
          const open = qc.getQueryData<{ feedId: number }>([
            "article",
            st.selectedArticleId,
          ]);
          if (open?.feedId === id) st.openArticle(null);
        }
      },
      commit: () => {
        makeDelete()
          .then(() => actions.refreshAfterBulk())
          .catch((e) => {
            restore();
            reportError(e);
          });
      },
      revert: restore,
    });
  };

  const allFeeds = feeds.data ?? [];
  const allFolders = folders.data ?? [];
  const allTags = tags.data ?? [];
  // Feeds section: interest vocabulary. Tags tab: AI-generated tags.
  const interestTags = allTags.filter(
    (tg) => (tg.kind ?? "interest") === "interest",
  );
  const aiTags = allTags.filter((tg) => tg.kind === "ai");
  // Preview on the Feeds tab: top 10 interest tags by article count.
  const topTags = [...interestTags]
    .sort(
      (a, b) =>
        b.articleCount - a.articleCount ||
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    )
    .slice(0, 10);
  const isActive = (q: ArticleQuery) => sameQuery(q, query);

  // Keep the selected feed in view. Opening an article from search jumps the
  // selection to a feed that may be scrolled out of sight — or hidden inside a
  // collapsed folder — so reveal it whenever the selection changes (issue #75).
  // `block: "nearest"` is a no-op when the row is already visible, so a plain
  // in-sidebar click never jolts the list.
  const activeFeedRef = useRef<HTMLDivElement>(null);
  // Remember which selection we last scrolled to, so a background feeds refetch
  // or an unrelated folder toggle (both re-run this effect) doesn't yank the
  // list back to the active feed.
  const lastScrolledRef = useRef("");
  useEffect(() => {
    const key = JSON.stringify(query);
    if (key === lastScrolledRef.current) return;
    if (query.kind === "feed") {
      const feed = allFeeds.find((f) => f.id === query.value);
      // A collapsed folder doesn't render its feed rows, so there's nothing to
      // scroll to yet — expand it and let this effect re-run (collapsed dep)
      // once the row is mounted.
      if (feed?.folderId != null && collapsed[feed.folderId]) {
        setCollapsed((s) => ({ ...s, [feed.folderId!]: false }));
        return;
      }
    }
    lastScrolledRef.current = key;
    activeFeedRef.current?.scrollIntoView({ block: "nearest" });
  }, [query, allFeeds, collapsed]);

  // "Unread only" hides feeds with nothing unread, decluttering large
  // sidebars. The currently-selected feed is always kept so it doesn't vanish
  // from under the user the moment its last article is marked read, and the
  // filter is suspended mid-drag so a drag target never disappears.
  const feedVisible = (f: Feed) =>
    !unreadOnly ||
    dragId != null ||
    f.unreadCount > 0 ||
    isActive({ kind: "feed", value: f.id });
  const visibleFeeds = allFeeds.filter(feedVisible);
  const ungrouped = sortFeeds(visibleFeeds.filter((f) => f.folderId == null));

  // ── drag to move a feed between folders ──
  const handleDrop = (target: number | null) => {
    if (!isAdmin) {
      setDragId(null);
      setDropFolder(null);
      return;
    }
    const feed = allFeeds.find((f) => f.id === dragId);
    setDragId(null);
    setDropFolder(null);
    if (!feed || feed.folderId === target) return;
    const folderName =
      target == null
        ? t("sidebar.uncategorized")
        : allFolders.find((f) => f.id === target)?.name ?? "";
    guard(
      api.moveFeed(feed.id, target),
      t("sidebar.toastMoved", { feed: feed.title, folder: folderName }),
    );
  };

  // ── feed / folder context menus ──
  const feedMenu = (f: Feed): MenuEntry[] => {
    const moves: MenuEntry[] = isAdmin
      ? allFolders
          .filter((fo) => fo.id !== f.folderId)
          .map((fo) => ({
            icon: "folder" as const,
            label: t("sidebar.moveToFolder", { folder: fo.name }),
            onClick: () =>
              guard(
                api.moveFeed(f.id, fo.id),
                t("sidebar.toastMovedTo", { folder: fo.name }),
              ),
          }))
      : [];
    if (isAdmin && f.folderId != null)
      moves.push({
        icon: "folder",
        label: t("sidebar.moveOutOfFolder"),
        onClick: () =>
          guard(api.moveFeed(f.id, null), t("sidebar.toastMovedOut")),
      });
    const items: MenuEntry[] = [
      {
        icon: "check-all",
        label: t("sidebar.markAllRead"),
        onClick: () =>
          guard(
            api.markAllRead({ kind: "feed", value: f.id }),
            t("sidebar.toastMarkedAllRead"),
          ),
      },
    ];
    if (isAdmin) {
      items.push({
        icon: "refresh",
        label: t("sidebar.refreshFeed"),
        onClick: () => onRefresh({ feedId: f.id }),
      });
      items.push({ separator: true });
      items.push({
        icon: "settings",
        label: t("sidebar.renameMenu"),
        onClick: () =>
          setPrompt({
            title: t("sidebar.renameFeedTitle"),
            initial: f.title,
            placeholder: t("sidebar.feedNamePlaceholder"),
            onSubmit: (v) =>
              guard(api.renameFeed(f.id, v), t("sidebar.toastRenamed")),
          }),
      });
      if (moves.length) items.push({ separator: true }, ...moves);
      items.push({ separator: true });
      items.push({
        icon: "trash",
        label: t("sidebar.unsubscribe"),
        danger: true,
        onClick: () =>
          guardDelete(
            () => api.deleteFeed(f.id),
            t("sidebar.toastUnsubscribed", { feed: f.title }),
            "feed",
            f.id,
          ),
      });
    }
    return items;
  };

  const folderMenu = (folder: Folder): MenuEntry[] => {
    const items: MenuEntry[] = [
      {
        icon: "check-all",
        label: t("sidebar.markAllRead"),
        onClick: () =>
          guard(
            api.markAllRead({ kind: "folder", value: folder.id }),
            t("sidebar.toastMarkedAllRead"),
          ),
      },
    ];
    if (isAdmin) {
      items.push({
        icon: "refresh",
        label: t("sidebar.refreshFolder"),
        onClick: () => onRefresh({ folderId: folder.id }),
      });
      items.push({ separator: true });
      items.push({
        icon: "settings",
        label: t("sidebar.renameMenu"),
        onClick: () =>
          setPrompt({
            title: t("sidebar.renameFolderTitle"),
            initial: folder.name,
            placeholder: t("sidebar.folderNamePlaceholder"),
            onSubmit: (v) =>
              guard(api.renameFolder(folder.id, v), t("sidebar.toastRenamed")),
          }),
      });
      items.push({ separator: true });
      items.push({
        icon: "trash",
        label: t("sidebar.deleteFolder"),
        danger: true,
        onClick: () =>
          guardDelete(
            () => api.deleteFolder(folder.id),
            t("sidebar.toastFolderDeleted"),
            "folder",
            folder.id,
          ),
      });
    }
    return items;
  };

  const tagMenu = (tag: Tag): MenuEntry[] => {
    if (!isAdmin) return [];
    return [
    {
      icon: "settings",
      label: t("sidebar.renameMenu"),
      onClick: () =>
        setPrompt({
          title: t("sidebar.renameTagTitle"),
          initial: tag.name,
          placeholder: t("sidebar.tagNamePlaceholder"),
          onSubmit: (v) =>
            guard(api.renameTag(tag.id, v), t("sidebar.toastRenamed")),
        }),
    },
    {
      swatches: Object.entries(TAG_PALETTE).map(([value, color]) => ({
        value,
        color,
      })),
      current: tag.color,
      // The recolour is instantly visible on the dot, so no toast — just
      // refresh the caches that embed the tag colour.
      onPick: (color) =>
        api
          .setTagColor(tag.id, color)
          .then(() => actions.refreshAfterBulk())
          .catch((e) => reportError(e)),
    },
    { separator: true },
    {
      icon: "trash",
      label: t("sidebar.deleteTag"),
      danger: true,
      onClick: () =>
        guardDelete(
          () => api.deleteTag(tag.id),
          t("sidebar.toastTagDeleted"),
          "tag",
          tag.id,
        ),
    },
    ];
  };

  // Creating a folder only touches the folders list, which `refreshAfterBulk`
  // (and thus `guard`) doesn't cover — invalidate it explicitly.
  const createFolder = () =>
    setPrompt({
      title: t("app.newFolderTitle"),
      initial: "",
      placeholder: t("app.folderNamePlaceholder"),
      onSubmit: (v) =>
        api
          .createFolder(v)
          .then(() => {
            qc.invalidateQueries({ queryKey: ["folders"] });
            onToast(t("app.folderCreated"));
          })
          .catch((e) => reportError(e)),
    });

  // Optimistically apply a new tag order, then persist; reconcile on either
  // outcome. Shared by the drag-reorder and the menu's move up/down.
  const persistTagOrder = (ordered: Tag[]) => {
    // Keep the other taxonomy's rows in the cache so a reorder of AI tags
    // doesn't briefly wipe interest tags from the Feeds section.
    const other = allTags.filter((tg) => !ordered.some((o) => o.id === tg.id));
    qc.setQueryData(["tags"], [...ordered, ...other]);
    api
      .reorderTags(ordered.map((tg) => tg.id))
      .catch((e) => reportError(e))
      .finally(() => qc.invalidateQueries({ queryKey: ["tags"] }));
  };

  // ── drag to reorder tags ──
  const dropTag = (targetId: number) => {
    const list = aiTags;
    const from = list.findIndex((tg) => tg.id === tagDragId);
    const to = list.findIndex((tg) => tg.id === targetId);
    setTagDragId(null);
    setTagOverId(null);
    if (from < 0 || to < 0 || from === to) return;
    const next = [...list];
    const [moved] = next.splice(from, 1);
    // The `drop-above` indicator marks an insertion point *before* the target
    // tag. After removing the dragged item, every index past `from` shifts
    // down by one — so a downward drag must insert at `to - 1` to land the
    // tag above the target, not below it.
    const insertAt = from < to ? to - 1 : to;
    next.splice(insertAt, 0, moved);
    persistTagOrder(next);
  };

  // ── feed row ──
  const feedRow = (f: Feed) => (
    <div
      key={f.id}
      ref={isActive({ kind: "feed", value: f.id }) ? activeFeedRef : undefined}
      className={`sb-item ${
        isActive({ kind: "feed", value: f.id }) ? "active" : ""
      } ${dragId === f.id ? "dragging" : ""}`}
      role="button"
      tabIndex={0}
      aria-current={isActive({ kind: "feed", value: f.id }) || undefined}
      draggable={isAdmin}
      onDragStart={() => {
        if (isAdmin) setDragId(f.id);
      }}
      onDragEnd={() => {
        setDragId(null);
        setDropFolder(null);
      }}
      onClick={() => select({ kind: "feed", value: f.id }, f.title)}
      onKeyDown={onActivate(() => select({ kind: "feed", value: f.id }, f.title))}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY, kind: "feed", feed: f });
      }}
      title={f.fetchError ?? f.title}
    >
      <FeedAvatar title={f.title} faviconUrl={f.faviconUrl} seed={f.id} />
      <span className="sb-label">{f.title}</span>
      {f.fetchError && (
        <span className="sb-warn" role="img" aria-label={t("sidebar.feedError")}>
          !
        </span>
      )}
      {showCounts && f.unreadCount > 0 && (
        <span className="sb-count">{f.unreadCount}</span>
      )}
    </div>
  );

  const onCloudTerm = (term: string, additive?: boolean) => {
    // Snapshot search before `select`, which clears listSearch on every browse
    // change — otherwise Shift+click would always see an empty set.
    const cur = useUi.getState().listSearch?.trim() ?? "";
    const parts = cur.split(/\s+/).filter(Boolean);
    let next: string | null;
    if (additive) {
      if (!cur) {
        next = term;
      } else if (parts.includes(term)) {
        next = parts.filter((p) => p !== term).join(" ") || null;
      } else {
        next = `${cur} ${term}`;
      }
    } else {
      next = term;
    }
    // Apply as the article-list filter, but stay on the word-cloud tab.
    select({ kind: "all" }, t("smart.all"));
    setListSearch(next);
  };

  /** Sidebar search → middle-column list filter (same path as word-cloud terms). */
  const applySidebarSearch = () => {
    const term = searchDraft.trim();
    if (!term) {
      setListSearch(null);
      return;
    }
    select({ kind: "all" }, t("smart.all"));
    setListSearch(term);
  };

  /** Tag row used on Feeds (preview) and Tags (full list, reorderable). */
  const tagRow = (tag: Tag, opts: { reorderable: boolean; alwaysCount: boolean }) => (
    <div
      key={tag.id}
      className={`sb-item ${
        isActive({ kind: "tag", value: tag.id }) ? "active" : ""
      } ${opts.reorderable && tagDragId === tag.id ? "dragging" : ""} ${
        opts.reorderable && tagOverId === tag.id ? "drop-above" : ""
      }`}
      role="button"
      tabIndex={0}
      aria-current={isActive({ kind: "tag", value: tag.id }) || undefined}
      draggable={opts.reorderable}
      onDragStart={
        opts.reorderable ? () => setTagDragId(tag.id) : undefined
      }
      onDragEnd={
        opts.reorderable
          ? () => {
              setTagDragId(null);
              setTagOverId(null);
            }
          : undefined
      }
      onDragOver={
        opts.reorderable
          ? (e) => {
              if (tagDragId != null && tagDragId !== tag.id) {
                e.preventDefault();
                setTagOverId(tag.id);
              }
            }
          : undefined
      }
      onDrop={opts.reorderable ? () => dropTag(tag.id) : undefined}
      onClick={() => select({ kind: "tag", value: tag.id }, tag.name)}
      onKeyDown={onActivate(() =>
        select({ kind: "tag", value: tag.id }, tag.name),
      )}
      onContextMenu={(e) => {
        e.preventDefault();
        if (!isAdmin) return;
        setMenu({ x: e.clientX, y: e.clientY, kind: "tag", tag });
      }}
    >
      <span className="sb-ico">
        <span
          className="tag-dot"
          style={{ background: tagColor(tag.color) }}
        />
      </span>
      <span className="sb-label">{tag.name}</span>
      {(opts.alwaysCount || showCounts) && tag.articleCount > 0 && (
        <span className="sb-count">{tag.articleCount}</span>
      )}
    </div>
  );

  return (
    <div className="sidebar" role="navigation">
      {/* <div className="sb-brand">
        <img className="sb-brand-mark" src="/papr.svg" alt="" />
        <span className="sb-brand-name">Papr</span>
      </div> */}

      <div className="sb-tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={sideTab === "feeds"}
          className={sideTab === "feeds" ? "active" : ""}
          onClick={() => setSideTab("feeds")}
        >
          <Icon name="rss" size={13} />
          <span>{t("sidebar.tabFeeds")}</span>
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={sideTab === "cloud"}
          className={sideTab === "cloud" ? "active" : ""}
          onClick={() => setSideTab("cloud")}
        >
          <Icon name="sparkle" size={13} />
          <span>{t("sidebar.tabCloud")}</span>
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={sideTab === "tags"}
          className={sideTab === "tags" ? "active" : ""}
          onClick={() => setSideTab("tags")}
        >
          <Icon name="tag" size={13} />
          <span>{t("sidebar.tabTags")}</span>
        </button>
      </div>

      {sideTab === "cloud" ? (
        <WordCloudPanel onSelectTerm={onCloudTerm} />
      ) : sideTab === "tags" ? (
        <div className="sidebar-scroll sb-tags-tab">
          <div className="sb-tags-card">
            <div className="sb-tags-card-head">
              <span>{t("sidebar.tabTags")}</span>
              <span className="sb-section-actions">
                <span
                  className="sb-feed-sort"
                  role="group"
                  aria-label={t("sidebar.sortTagsBy")}
                >
                  <button
                    type="button"
                    className={tagSort.mode === "alpha" ? "active" : ""}
                    onClick={() => setTagSortMode("alpha")}
                    aria-pressed={tagSort.mode === "alpha"}
                    title={t("sidebar.sortAlphaHint")}
                  >
                    {tagSort.mode === "alpha" && tagSort.dir === "desc"
                      ? "Z-A ↓"
                      : "A-Z ↑"}
                  </button>
                  <span className="sb-feed-sort-sep" aria-hidden="true">
                    ·
                  </span>
                  <button
                    type="button"
                    className={tagSort.mode === "count" ? "active" : ""}
                    onClick={() => setTagSortMode("count")}
                    aria-pressed={tagSort.mode === "count"}
                    title={t("sidebar.sortCountHint")}
                  >
                    {t("sidebar.sortCount")}{" "}
                    {tagSort.mode === "count" && tagSort.dir === "asc"
                      ? "↑"
                      : "↓"}
                  </button>
                </span>
              </span>
            </div>
            {aiTags.length === 0 ? (
              <div className="sb-tags-empty">{t("sidebar.aiTagsEmptyHint")}</div>
            ) : (
              sortTags(aiTags).map((tag) =>
                tagRow(tag, {
                  // Client-side sort owns display order; drag-reorder would
                  // fight the selected mode (same idea as the Feeds list).
                  reorderable: false,
                  alwaysCount: true,
                }),
              )
            )}
          </div>
          <div style={{ height: 20 }} />
        </div>
      ) : (
        <>
      <label className="sidebar-search">
        <Icon name="search" size={13} />
        <input
          type="search"
          value={searchDraft}
          placeholder={t("sidebar.searchArticles")}
          aria-label={t("sidebar.searchArticles")}
          onChange={(e) => setSearchDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              applySidebarSearch();
            }
          }}
        />
      </label>

      <div className="sidebar-scroll">
        <div className="sb-section-title">
          <span>{t("sidebar.library")}</span>
        </div>
        <SbItem
          icon="inbox"
          label={t("smart.all")}
          active={isActive({ kind: "all" })}
          onClick={() => select({ kind: "all" }, t("smart.all"))}
        />
        <SbItem
          icon="unread"
          label={t("smart.unread")}
          count={showCounts ? counts.data?.unread : undefined}
          active={isActive({ kind: "unread" })}
          onClick={() => select({ kind: "unread" }, t("smart.unread"))}
        />
        <SbItem
          icon="star"
          label={t("smart.starred")}
          count={showCounts ? counts.data?.starred : undefined}
          active={isActive({ kind: "starred" })}
          onClick={() => select({ kind: "starred" }, t("smart.starred"))}
        />
        <SbItem
          icon="bookmark"
          label={t("smart.readLater")}
          count={showCounts ? counts.data?.readLater : undefined}
          active={isActive({ kind: "readLater" })}
          onClick={() => select({ kind: "readLater" }, t("smart.readLater"))}
        />

        <div className="sb-section-title">
          <span>{t("sidebar.feeds")}</span>
          <span className="sb-section-actions">
            <span
              className="sb-feed-sort"
              role="group"
              aria-label={t("sidebar.sortBy")}
            >
              <button
                type="button"
                className={feedSort.mode === "alpha" ? "active" : ""}
                onClick={() => setSortMode("alpha")}
                aria-pressed={feedSort.mode === "alpha"}
                title={t("sidebar.sortAlphaHint")}
              >
                {feedSort.mode === "alpha" && feedSort.dir === "desc"
                  ? "Z-A ↓"
                  : "A-Z ↑"}
              </button>
              <span className="sb-feed-sort-sep" aria-hidden="true">
                ·
              </span>
              <button
                type="button"
                className={feedSort.mode === "unread" ? "active" : ""}
                onClick={() => setSortMode("unread")}
                aria-pressed={feedSort.mode === "unread"}
                title={t("sidebar.sortUnreadHint")}
              >
                {t("sidebar.sortUnread")}{" "}
                {feedSort.mode === "unread" && feedSort.dir === "asc"
                  ? "↑"
                  : "↓"}
              </button>
            </span>
            <button
              className={unreadOnly ? "active" : ""}
              onClick={() => setPref({ sidebarUnreadOnly: !unreadOnly })}
              title={t("sidebar.unreadOnly")}
              aria-label={t("sidebar.unreadOnly")}
              aria-pressed={unreadOnly}
            >
              <Icon name={unreadOnly ? "eye-off" : "eye"} size={12} />
            </button>
            {isAdmin && (
              <>
                <button
                  onClick={createFolder}
                  title={t("app.newFolderTitle")}
                  aria-label={t("app.newFolderTitle")}
                >
                  <Icon name="folder" size={12} />
                </button>
                <button
                  onClick={onAddFeed}
                  title={t("sidebar.addFeed")}
                  aria-label={t("sidebar.addFeed")}
                >
                  <Icon name="plus" size={12} />
                </button>
              </>
            )}
          </span>
        </div>

        {allFeeds.length === 0 && (
          <div
            style={{
              padding: "10px 12px",
              fontSize: 12,
              color: "var(--muted)",
              lineHeight: 1.5,
            }}
          >
            {t("sidebar.emptyHint")}
          </div>
        )}

        {allFeeds.length > 0 && unreadOnly && visibleFeeds.length === 0 && (
          <div
            style={{
              padding: "10px 12px",
              fontSize: 12,
              color: "var(--muted)",
              lineHeight: 1.5,
            }}
          >
            {t("sidebar.allReadHint")}
          </div>
        )}

        {/* Ungrouped feeds — also the drop zone for "move out of folder".
            Rendered whenever there are ungrouped feeds, OR while a feed drag
            is in progress: without the latter, a drag-to-ungroup would have
            no target to land on once every feed lives inside a folder. */}
        {(ungrouped.length > 0 ||
          (dragId != null &&
            allFeeds.find((f) => f.id === dragId)?.folderId != null)) && (
          <div
            onDragOver={(e) => {
              if (dragId != null) {
                e.preventDefault();
                setDropFolder("none");
              }
            }}
            onDrop={() => handleDrop(null)}
            style={
              dropFolder === "none"
                ? { outline: "2px solid var(--accent)", borderRadius: 8 }
                : undefined
            }
          >
            {ungrouped.length > 0 ? (
              ungrouped.map(feedRow)
            ) : (
              <div className="sb-drop-hint">{t("sidebar.dropUngroup")}</div>
            )}
          </div>
        )}

        {allFolders.map((folder) => {
          const inFolder = sortFeeds(
            visibleFeeds.filter((f) => f.folderId === folder.id),
          );
          // In "unread only" mode an empty folder is hidden — unless a drag is
          // active, when it must stay as a drop target.
          if (unreadOnly && dragId == null && inFolder.length === 0)
            return null;
          const isCollapsed = collapsed[folder.id];
          const folderActive = isActive({ kind: "folder", value: folder.id });
          // Aggregate unread for the folder. Shown while collapsed or when the
          // folder is the active view — expanded *and* not selected, the
          // per-feed badges already carry the same signal and a header total
          // would just duplicate them.
          const folderUnread = inFolder.reduce((n, f) => n + f.unreadCount, 0);
          return (
            <div
              key={folder.id}
              onDragOver={(e) => {
                if (dragId != null) {
                  e.preventDefault();
                  setDropFolder(folder.id);
                }
              }}
              onDrop={() => handleDrop(folder.id)}
              style={
                dropFolder === folder.id
                  ? { outline: "2px solid var(--accent)", borderRadius: 8 }
                  : undefined
              }
            >
              <div
                className={`sb-folder ${isCollapsed ? "collapsed" : ""} ${
                  folderActive ? "active" : ""
                }`}
                role="button"
                tabIndex={0}
                aria-expanded={!isCollapsed}
                aria-current={folderActive || undefined}
                aria-label={`${folder.name}, ${t(
                  isCollapsed ? "sidebar.expandFolder" : "sidebar.collapseFolder",
                )}`}
                onClick={() =>
                  setCollapsed((s) => ({ ...s, [folder.id]: !isCollapsed }))
                }
                onKeyDown={onActivate(() =>
                  setCollapsed((s) => ({ ...s, [folder.id]: !isCollapsed })),
                )}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setMenu({ x: e.clientX, y: e.clientY, kind: "folder", folder });
                }}
              >
                {/* Chevron, name, and the row body toggle expand/collapse.
                    The unread count (below) selects the folder's article view. */}
                <span className="sb-folder-toggle" aria-hidden>
                  <Icon name="chevron-down" size={11} />
                </span>
                <span className="sb-folder-name">{folder.name}</span>
                {showCounts && (isCollapsed || folderActive) && folderUnread > 0 && (
                  <span
                    className="sb-count"
                    role="button"
                    tabIndex={0}
                    aria-label={folder.name}
                    onClick={(e) => {
                      e.stopPropagation();
                      select(
                        { kind: "folder", value: folder.id },
                        folder.name,
                      );
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        e.stopPropagation();
                        select(
                          { kind: "folder", value: folder.id },
                          folder.name,
                        );
                      }
                    }}
                  >
                    {folderUnread}
                  </span>
                )}
              </div>
              {!isCollapsed && inFolder.map(feedRow)}
            </div>
          );
        })}

        {topTags.length > 0 && (
          <>
            <div className="sb-section-title">
              <span>{t("sidebar.interestTags")}</span>
            </div>
            {topTags.map((tag) =>
              tagRow(tag, { reorderable: false, alwaysCount: false }),
            )}
          </>
        )}

        <div style={{ height: 30 }} />
      </div>
        </>
      )}

      <div className="sb-footer">
        {isAdmin && (
          <button
            title={t("sidebar.addFeedShortcut")}
            aria-label={t("sidebar.addFeedShortcut")}
            onClick={onAddFeed}
          >
            <Icon name="plus" size={14} />
          </button>
        )}
        {isAdmin && (
          <button
            title={t("sidebar.refreshAll")}
            aria-label={t("sidebar.refreshAll")}
            onClick={() => onRefresh()}
            disabled={refreshing}
            className={refreshing ? "spinning" : ""}
          >
            <Icon name="refresh" size={14} />
          </button>
        )}
        {isAdmin && (
          <button
            title={t("sidebar.explore")}
            aria-label={t("sidebar.explore")}
            onClick={onExplore}
          >
            <Icon name="globe" size={14} />
          </button>
        )}
        <div className="spacer" />
        <button
          title={t("sidebar.settings")}
          aria-label={t("sidebar.settings")}
          onClick={() => onOpenSettings()}
        >
          <Icon name="settings" size={14} />
        </button>
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={
            menu.kind === "feed"
              ? feedMenu(menu.feed)
              : menu.kind === "folder"
                ? folderMenu(menu.folder)
                : tagMenu(menu.tag)
          }
          onClose={() => setMenu(null)}
        />
      )}
      {prompt && (
        <PromptDialog
          title={prompt.title}
          initialValue={prompt.initial}
          placeholder={prompt.placeholder}
          onSubmit={prompt.onSubmit}
          onClose={() => setPrompt(null)}
        />
      )}
    </div>
  );
}
