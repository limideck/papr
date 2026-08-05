// Shared article mutations. Both the reading pane and keyboard shortcuts go
// through this so optimistic cache patching stays consistent everywhere.

import { useQueryClient } from "@tanstack/react-query";
import * as api from "../api";
import { errorText } from "../lib/errors";
import type { ArticleSummary } from "../types";

type Patch = Partial<
  Pick<ArticleSummary, "isRead" | "isStarred" | "readLater">
>;

/**
 * Shared article mutations. `onError` (when supplied) is called with a
 * localized message if a mutation fails — callers fire these actions without
 * awaiting, so without it a failure would be a silent unhandled rejection.
 */
export function useArticleActions(onError?: (msg: string) => void) {
  const qc = useQueryClient();

  /** Optimistically patch an article across every cache that may hold it. */
  const patch = (id: number, p: Patch) => {
    // Paginated browse lists.
    qc.setQueriesData({ queryKey: ["articles"] }, (old: any) => {
      if (!old?.pages) return old;
      return {
        ...old,
        pages: old.pages.map((page: ArticleSummary[]) =>
          page.map((x) => (x.id === id ? { ...x, ...p } : x)),
        ),
      };
    });
    // Flat result arrays: hybrid search and the command-palette search.
    const patchFlat = (old: any) =>
      Array.isArray(old)
        ? old.map((x: ArticleSummary) => (x.id === id ? { ...x, ...p } : x))
        : old;
    qc.setQueriesData({ queryKey: ["search"] }, patchFlat);
    qc.setQueriesData({ queryKey: ["cp-search"] }, patchFlat);
    // The open article detail.
    qc.setQueryData(["article", id], (old: any) =>
      old ? { ...old, ...p } : old,
    );
  };

  const refreshLists = () => {
    qc.invalidateQueries({ queryKey: ["counts"] });
    qc.invalidateQueries({ queryKey: ["feeds"] });
    // Smart views (Starred / Read Later / Unread) are each their own
    // ["articles", …] query. The optimistic `patch` above fixes articles
    // already in a list, but it can't add or remove rows — so a freshly
    // starred article never appears in the Starred list. Mark every
    // article/search list stale so it re-fetches with the correct
    // membership when next opened. `refetchType: "none"` avoids yanking
    // rows out of the list the user is currently looking at.
    qc.invalidateQueries({ queryKey: ["articles"], refetchType: "none" });
    qc.invalidateQueries({ queryKey: ["search"], refetchType: "none" });
    qc.invalidateQueries({ queryKey: ["cp-search"], refetchType: "none" });
  };

  // The query keys an article-state change can affect. A bare
  // `invalidateQueries()` would also refetch unrelated caches (AI summaries,
  // settings, FreshRSS status, rules, the feed-discovery search), so callers
  // invalidate only these — plus any `extra` keys.
  const refreshArticleKeys = (extra: string[][] = []) => {
    const keys = [
      ["counts"],
      ["feeds"],
      ["folders"],
      ["tags"],
      ["articles"],
      ["article"],
      ["search"],
      ["cp-search"],
      ...extra,
    ];
    for (const key of keys) {
      qc.invalidateQueries({ queryKey: key });
    }
  };

  // After a bulk operation (mark-all-read) potentially every article's state
  // changed, so optimistic patching can't cover it.
  const refreshAfterBulk = () => refreshArticleKeys();

  // After a manual feed refresh new articles may have arrived; `storage-stats`
  // is added because the new articles grow the database.
  const refreshAfterFetch = () => refreshArticleKeys([["storage-stats"]]);

  return {
    patch,
    refreshAfterBulk,
    refreshAfterFetch,
    async setRead(id: number, read: boolean) {
      try {
        await api.markRead(id, read);
        patch(id, { isRead: read });
        refreshLists();
      } catch (e) {
        onError?.(errorText(e));
      }
    },
    async setStarred(id: number, starred: boolean) {
      try {
        await api.markStarred(id, starred);
        patch(id, { isStarred: starred });
        refreshLists();
      } catch (e) {
        onError?.(errorText(e));
      }
    },
    async setReadLater(id: number, value: boolean) {
      try {
        await api.markReadLater(id, value);
        patch(id, { readLater: value });
        refreshLists();
      } catch (e) {
        onError?.(errorText(e));
      }
    },
  };
}
