/* Tag management workbench — tree-first two-level taxonomy editing.
 * P1+P2 of docs/tag-management-design.md. Mounted by the sidebar.
 */
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  confirmReviewTag,
  createTag,
  createTagAlias,
  getTagAdmin,
  deleteTag,
  deleteTagAlias,
  listSuppressedTags,
  listTagAliases,
  listTags,
  mergeTags,
  renameTag,
  reviewQueue,
  setTagHierarchy,
  suppressTag,
  unsuppressTag,
  type ReviewFilter,
} from "../api";
import type { ReviewQueueItem, SuppressedTagItem, Tag, TagKind } from "../types";

type Filter = "all" | "parents" | "untyped";
type ViewMode = "tree" | "review" | "suppressed";
const QUEUE_PAGE = 50;
const DOMAINS = [
  "Economy","Finance","National Security","Geopolitics","Technology","Society",
] as const;
type TypeView = "all" | "entity" | "topic" | "none" | "aliased";

interface Props {
  open: boolean;
  onClose: () => void;
}

export default function TagManageDialog({ open, onClose }: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [kind, setKind] = useState<TagKind>("ai");
  const [search, setSearch] = useState("");
  const [minUse, setMinUse] = useState(0);
  const [filter, setFilter] = useState<Filter>("all");
  const [typeView, setTypeView] = useState<TypeView>("all");
  const [adding, setAdding] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<number>>(new Set());
  const [selected, setSelected] = useState<number | null>(null);
  const [mergingFrom, setMergingFrom] = useState<number | null>(null);
  const [pickingParent, setPickingParent] = useState(false);
  const [view, setView] = useState<ViewMode>("tree");
  const [reviewFilter, setReviewFilter] = useState<ReviewFilter>("new");
  const [reviewPage, setReviewPage] = useState(0);
  const [suppPage, setSuppPage] = useState(0);

  const tagsQ = useQuery({
    queryKey: ["tags-manage", kind],
    queryFn: () => listTags(kind),
    enabled: open,
  });
  const newQ = useQuery({
    queryKey: ["tag-new-ids"],
    queryFn: async () => {
      const r = await reviewQueue("new", 0, 200);
      return new Set(r.items.map((i) => i.id));
    },
    enabled: open,
  });
  const suppQ = useQuery({
    queryKey: ["tag-suppressed-ids"],
    queryFn: async () => {
      const r = await listSuppressedTags(0, 500);
      return new Set(r.items.map((i) => i.id));
    },
    enabled: open,
  });

  const queueQ = useQuery({
    queryKey: ["tag-review-queue", reviewFilter, reviewPage],
    queryFn: () =>
      reviewQueue(reviewFilter, reviewPage * QUEUE_PAGE, QUEUE_PAGE),
    enabled: open && view === "review",
  });
  const suppListQ = useQuery({
    queryKey: ["tag-suppressed-list", suppPage],
    queryFn: () => listSuppressedTags(suppPage * QUEUE_PAGE, QUEUE_PAGE),
    enabled: open && view === "suppressed",
  });
  const countsQ = useQuery({
    queryKey: ["tag-queue-counts"],
    queryFn: async () => {
      const [rv, sp] = await Promise.all([
        reviewQueue("new", 0, 1),
        listSuppressedTags(0, 1),
      ]);
      return { review: rv.total, suppressed: sp.total };
    },
    enabled: open,
  });

  const aliasAllQ = useQuery({
    queryKey: ["tag-aliases-all", kind],
    queryFn: () => listTagAliases({ kind }),
    enabled: open,
  });
  const aliasTagIds = useMemo(() => {
    const m = new Set<number>();
    for (const a of aliasAllQ.data ?? []) m.add(a.tagId);
    return m;
  }, [aliasAllQ.data]);

  const tags = tagsQ.data ?? [];
  const byId = useMemo(() => new Map(tags.map((t) => [t.id, t])), [tags]);

  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    return tags.filter((t) => {
      if (t.articleCount < minUse) return false;
      if (typeView === "entity" && t.tagType !== "entity") return false;
      if (typeView === "topic" && t.tagType !== "topic") return false;
      if (typeView === "none" && t.tagType != null) return false;
      if (typeView === "aliased" && !aliasTagIds.has(t.id)) return false;
      if (filter === "parents" && !tags.some((c) => c.parentId === t.id)) return false;
      if (filter === "untyped" && t.tagType) return false;
      if (q && !t.name.toLowerCase().includes(q)) return false;
      return true;
    });
  }, [tags, search, minUse, filter, typeView, aliasTagIds]);

  const childMap = useMemo(() => {
    const m = new Map<number, Tag[]>();
    for (const t of visible) {
      if (t.parentId != null) {
        const arr = m.get(t.parentId) ?? [];
        arr.push(t);
        m.set(t.parentId, arr);
      }
    }
    return m;
  }, [visible]);
  const topLevel = visible.filter((t) => t.parentId == null);
  const typeCounts = useMemo(() => {
    const c = { entity: 0, topic: 0, none: 0 };
    for (const t of tags) {
      if (t.tagType === "entity") c.entity++;
      else if (t.tagType === "topic") c.topic++;
      else c.none++;
    }
    return c;
  }, [tags]);

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: ["tags-manage"] });
    void qc.invalidateQueries({ queryKey: ["tags"] });
    void qc.invalidateQueries({ queryKey: ["tag-new-ids"] });
    void qc.invalidateQueries({ queryKey: ["tag-suppressed-ids"] });
    void qc.invalidateQueries({ queryKey: ["tag-queue-counts"] });
    void qc.invalidateQueries({ queryKey: ["tag-review-queue"] });
    void qc.invalidateQueries({ queryKey: ["tag-suppressed-list"] });
    void qc.invalidateQueries({ queryKey: ["tag-aliases"] });
  };

  const toggle = (id: number) =>
    setCollapsed((p) => {
      const n = new Set(p);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const subtreeExtra = (id: number) => {
    let n = 0;
    for (const c of childMap.get(id) ?? []) n += c.articleCount;
    return n;
  };


  const err = (e: unknown) =>
    window.alert(String((e as { message?: string })?.message ?? e));

  const doMerge = (fromId: number, toId: number) =>
    void mergeTags(fromId, toId).then(() => {
      setMergingFrom(null);
      invalidate();
    });
  const doType = (id: number, tagType: "entity" | "topic" | null) =>
    void setTagHierarchy(id, { tagType }).then(() => invalidate()).catch(err);

  const selectedTag = selected != null ? byId.get(selected) : undefined;
  const adminQ = useQuery({
    queryKey: ["tag-admin", selected],
    queryFn: () => getTagAdmin(selected as number),
    enabled: selected != null,
  });
  const admin = adminQ.data;

  const addNew = async (name: string, typeSel: "entity" | "topic" | null, parentName?: string) => {
    try {
      const id = await createTag(name, kind);
      if (kind === "ai" && (typeSel != null || parentName)) {
        await setTagHierarchy(id, {
          ...(typeSel != null ? { tagType: typeSel } : {}),
          ...(parentName ? { parentName } : {}),
        });
      }
      setAdding(false);
      setSelected(id);
      invalidate();
    } catch (e) {
      err(e);
    }
  };

  if (!open) return null;

  const parentChoices = tags.filter(
    (t) => t.kind === "ai" && t.tagType === "topic" && t.id !== selected,
  );

  // ── tree row (kept from the P1 browse view) ──────────────
  const row = (tag: Tag, depth: number) => {
    const kids = childMap.get(tag.id) ?? [];
    const expanded = !collapsed.has(tag.id);
    const isNew = newQ.data?.has(tag.id) ?? false;
    const suppressed = suppQ.data?.has(tag.id) ?? false;
    const extra = subtreeExtra(tag.id) - tag.articleCount;
    return (
      <div key={tag.id}>
        <div
          className={["tgm-row", `d${depth}`, selected === tag.id ? "sel" : ""].join(" ")}
          onClick={() => {
            if (mergingFrom != null) {
              if (mergingFrom !== tag.id) {
                if (window.confirm(`${t("tagManage.mergeInto")}「${tag.name}」？`))
                  doMerge(mergingFrom, tag.id);
              }
              return;
            }
            setSelected(tag.id === selected ? null : tag.id);
          }}
        >
          <span className="tgm-arrow" onClick={(e) => { e.stopPropagation(); toggle(tag.id); }}>
            {kids.length > 0 ? (expanded ? "▾" : "▸") : ""}
          </span>
          <span className="tgm-kind-dot" data-type={tag.tagType ?? "none"} />
          <span className="tgm-name">{tag.name}</span>
          {tag.tagType && <span className="tgm-type">{tag.tagType}</span>}
          {extra > 0 && <span className="tgm-sub">+{extra}</span>}
          <span className="tgm-count">{tag.articleCount}</span>
          {isNew && <span className="tgm-badge new">{t("tagManage.new")}</span>}
          {suppressed && <span className="tgm-badge supp">×</span>}
          <button
            className="tgm-act"
            title={t("tagManage.mergeTo")}
            onClick={(e) => {
              e.stopPropagation();
              setMergingFrom(mergingFrom === tag.id ? null : tag.id);
            }}
          >
            ⤳
          </button>
        </div>
        {kids.length > 0 && expanded && kids.map((c) => row(c, depth + 1))}
      </div>
    );
  };

  // ── queue-view actions ────────────────────────────────────
  const switchView = (v: ViewMode) => {
    setView(v);
    setSelected(null);
    setMergingFrom(null);
    setPickingParent(false);
    setAdding(false);
  };

  const confirmReviewItem = (item: ReviewQueueItem) => {
    confirmReviewTag(item.id)
      .then(() => {
        invalidate();
        if (selected === item.id) setSelected(null);
      })
      .catch(err);
  };
  const suppressReviewItem = (item: ReviewQueueItem) => {
    suppressTag(item.id)
      .then(() => {
        invalidate();
        if (selected === item.id) setSelected(null);
      })
      .catch(err);
  };
  const deleteReviewItem = (item: ReviewQueueItem) => {
    if (
      !window.confirm(`${t("settings.autoTag.deleteAiConfirm", { name: item.name })}`)
    )
      return;
    deleteTag(item.id)
      .then(() => {
        invalidate();
        if (selected === item.id) setSelected(null);
      })
      .catch(err);
  };
  const queueType = (id: number, ty: "entity" | "topic" | null) => {
    setTagHierarchy(id, { tagType: ty }).then(() => invalidate()).catch(err);
  };
  const restoreItem = (item: SuppressedTagItem) => {
    unsuppressTag(item.id).then(() => invalidate()).catch(err);
  };
  const setReviewFilterAndReset = (f: ReviewFilter) => {
    setReviewFilter(f);
    setReviewPage(0);
  };

  const rq = queueQ.data;
  const reviewItems = rq?.items ?? [];
  const reviewPages = Math.max(1, Math.ceil((rq?.total ?? 0) / QUEUE_PAGE));
  const sq = suppListQ.data;
  const suppItems = sq?.items ?? [];
  const suppPages = Math.max(1, Math.ceil((sq?.total ?? 0) / QUEUE_PAGE));

  const counts = countsQ.data;

  return (
    <div className="tgm-overlay" onClick={onClose}>
      <div className="tgm" onClick={(e) => e.stopPropagation()}>
        <header className="tgm-head">
          <h2>{t("tagManage.title")}</h2>
          <nav className="tgm-viewtabs" role="tablist" aria-label={t("tagManage.tabBrowse")}>
            {(
              [
                ["tree", t("tagManage.tabBrowse")],
                [
                  "review",
                  counts && counts.review > 0
                    ? `${t("tagManage.tabReview")} (${counts.review})`
                    : t("tagManage.tabReview"),
                ],
                [
                  "suppressed",
                  counts && counts.suppressed > 0
                    ? `${t("tagManage.tabSuppressed")} (${counts.suppressed})`
                    : t("tagManage.tabSuppressed"),
                ],
              ] as [ViewMode, string][]
            ).map(([v, label]) => (
              <button
                key={v}
                type="button"
                role="tab"
                aria-selected={view === v}
                className={view === v ? "active" : ""}
                onClick={() => switchView(v)}
              >
                {label}
              </button>
            ))}
          </nav>
          {view === "tree" && (
            <div className="tgm-kind-switch">
              {(["ai", "interest"] as TagKind[]).map((k) => (
                <button
                  key={k}
                  className={kind === k ? "active" : ""}
                  onClick={() => { setKind(k); setSelected(null); }}
                >
                  {k === "ai" ? t("tagManage.aiTags") : t("tagManage.interestTags")}
                </button>
              ))}
            </div>
          )}
          {view === "tree" && (
            <div className="tgm-typeview">
              {([
                ["all", t("tagManage.viewAll")],
                ["entity", `${t("tagManage.viewEntity")} ${typeCounts.entity}`],
                ["topic", `${t("tagManage.viewTopic")} ${typeCounts.topic}`],
                ["none", `${t("tagManage.viewUntyped")} ${typeCounts.none}`],
                ["aliased", `${t("tagManage.viewAliased")} ${aliasTagIds.size}`],
              ] as [TypeView, string][]).map(([v, label]) => (
                <button
                  key={v}
                  className={typeView === v ? "active" : ""}
                  onClick={() => setTypeView(v)}
                >
                  {label}
                </button>
              ))}
            </div>
          )}
          {view === "tree" && (
            <button className="tgm-add" onClick={() => setAdding((a) => !a)}>
              ＋ {t("tagManage.addNew")}
            </button>
          )}
          {view === "tree" && (
            <div className="tgm-toolbar">
              <input
                className="tgm-search"
                placeholder={t("tagManage.search")}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
              <select value={minUse} onChange={(e) => setMinUse(Number(e.target.value))}>
                <option value={0}>{t("tagManage.allUse")}</option>
                {[1, 5, 10, 20].map((v) => (
                  <option key={v} value={v}>≥{v}</option>
                ))}
              </select>
              <select value={filter} onChange={(e) => setFilter(e.target.value as Filter)}>
                <option value="all">{t("tagManage.filterAll")}</option>
                <option value="parents">{t("tagManage.filterParents")}</option>
                <option value="untyped">{t("tagManage.filterUntyped")}</option>
              </select>
            </div>
          )}
          <button className="tgm-close" onClick={onClose}>✕</button>
        </header>

        {view === "tree" ? (
          <div className="tgm-body">
            <div className="tgm-tree">
              {adding && (
                <AddTagForm
                  kind={kind}
                  topics={tags.filter((t) => t.kind === "ai" && t.tagType === "topic")}
                  onCancel={() => setAdding(false)}
                  onSubmit={(n, ty, pa) => void addNew(n, ty, pa)}
                />
              )}
              {mergingFrom != null && (
                <div className="tgm-hint">{t("tagManage.pickMergeTarget")}</div>
              )}
              {topLevel.map((t) => row(t, 0))}
              {topLevel.length === 0 && (
                <div className="tgm-empty">{t("tagManage.empty")}</div>
              )}
            </div>
            <aside className="tgm-detail">
              {!selectedTag && (
                <div className="tgm-detail-empty">
                  {mergingFrom ? t("tagManage.pickMergeTarget") : t("tagManage.selectHint")}
                </div>
              )}
              {selectedTag && (
                <div className="tgm-card">
                  <h3>{selectedTag.name}</h3>
                  <dl>
                    <dt>{t("tagManage.usage")}</dt><dd>{selectedTag.articleCount}</dd>
                    <dt>{t("tagManage.type")}</dt>
                    <dd>{selectedTag.tagType ?? "—"}</dd>
                    <dt>{t("tagManage.parent")}</dt>
                    <dd>
                      {selectedTag.parentId != null
                        ? byId.get(selectedTag.parentId)?.name ?? "?"
                        : "—"}
                    </dd>
                    <dt>{t("tagManage.domain")}</dt>
                    <dd>{admin?.domain ?? "—"}</dd>
                    {suppQ.data?.has(selectedTag.id) && (
                      <>
                        <dt>{t("tagManage.suppressed")}</dt><dd>✓</dd>
                      </>
                    )}
                  </dl>
                  <div className="tgm-card-actions">
                    <button
                      onClick={() => {
                        const name = window.prompt(t("tagManage.renamePrompt"), selectedTag.name);
                        if (name && name.trim() !== selectedTag.name)
                          void renameTag(selectedTag.id, name.trim()).then(() => invalidate()).catch(err);
                      }}
                    >
                      {t("tagManage.rename")}
                    </button>
                    <button
                      onClick={() => setMergingFrom(mergingFrom === selectedTag.id ? null : selectedTag.id)}
                    >
                      {t("tagManage.mergeTo")}
                    </button>
                    {pickingParent ? (
                      <div className="tgm-parent-pick">
                        <select
                          autoFocus
                          defaultValue=""
                          onChange={(e) => {
                            const name = e.target.value;
                            setPickingParent(false);
                            if (name)
                              void setTagHierarchy(selectedTag.id, { parentName: name }).then(() => invalidate()).catch(err);
                          }}
                        >
                          <option value="">{t("tagManage.chooseParent")}</option>
                          {parentChoices.map((p) => (
                            <option key={p.id} value={p.name}>
                              {p.name} ({p.articleCount})
                            </option>
                          ))}
                        </select>
                      </div>
                    ) : (
                      <button onClick={() => setPickingParent(true)}>{t("tagManage.setParent")}</button>
                    )}
                    {selectedTag.parentId != null && (
                      <button
                        onClick={() => void setTagHierarchy(selectedTag.id, { parentName: null }).then(() => invalidate())}
                      >
                        {t("tagManage.clearParent")}
                      </button>
                    )}
                    {selectedTag.tagType !== "entity" && (
                      <button onClick={() => doType(selectedTag.id, "entity")}>{t("tagManage.asEntity")}</button>
                    )}
                    {selectedTag.tagType !== "topic" && (
                      <button onClick={() => doType(selectedTag.id, "topic")}>{t("tagManage.asTopic")}</button>
                    )}
                    {selectedTag.tagType != null && (
                      <button onClick={() => doType(selectedTag.id, null)}>{t("tagManage.clearType")}</button>
                    )}
                    <select
                      className="tgm-domain"
                      value={admin?.domain ?? ""}
                      onChange={(e) => {
                        const v = e.target.value;
                        void setTagHierarchy(selectedTag.id, { domain: v || null })
                          .then(() => invalidate())
                          .catch(err);
                      }}
                    >
                      <option value="">{t("tagManage.noDomain")}</option>
                      {DOMAINS.map((d) => (
                        <option key={d} value={d}>
                          {d}
                        </option>
                      ))}
                    </select>
                    {suppQ.data?.has(selectedTag.id) ? (
                      <button onClick={() => void unsuppressTag(selectedTag.id).then(() => invalidate())}>
                        {t("tagManage.unsuppress")}
                      </button>
                    ) : (
                      <button onClick={() => void suppressTag(selectedTag.id).then(() => invalidate())}>
                        {t("tagManage.suppress")}
                      </button>
                    )}
                    {newQ.data?.has(selectedTag.id) && (
                      <button
                        onClick={() => void confirmReviewTag(selectedTag.id).then(() => invalidate())}
                      >
                        {t("tagManage.confirmReviewed")}
                      </button>
                    )}
                    <button
                      className="danger"
                      onClick={() => {
                        if (window.confirm(`${t("tagManage.deleteConfirm")}「${selectedTag.name}」(${selectedTag.articleCount} 篇)`)) {
                          void deleteTag(selectedTag.id).then(() => { setSelected(null); invalidate(); }).catch(err);
                        }
                      }}
                    >
                      {t("tagManage.delete")}
                    </button>
                  </div>
                  <AliasEditor tagId={selectedTag.id} invalidate={invalidate} />
                </div>
              )}
            </aside>
          </div>
        ) : view === "review" ? (
          <div className="tgm-queue">
            <div className="tgm-queue-head">
              <div
                className="tgm-queue-filters"
                role="group"
                aria-label={t("settings.autoTag.reviewFilterLabel")}
              >
                {(["new", "single", "unparented", "all"] as ReviewFilter[]).map((f) => (
                  <button
                    key={f}
                    type="button"
                    className={reviewFilter === f ? "active" : ""}
                    onClick={() => setReviewFilterAndReset(f)}
                    aria-pressed={reviewFilter === f}
                  >
                    {t(`settings.autoTag.reviewFilter${f[0].toUpperCase()}${f.slice(1)}`)}
                  </button>
                ))}
              </div>
              {reviewPages > 1 && (
                <div className="tgm-pager">
                  <button
                    disabled={reviewPage <= 0}
                    onClick={() => setReviewPage((p) => Math.max(0, p - 1))}
                  >
                    {t("settings.autoTag.prevPage")}
                  </button>
                  <span>
                    {t("settings.autoTag.pageOf", {
                      current: reviewPage + 1,
                      total: reviewPages,
                    })}
                  </span>
                  <button
                    disabled={reviewPage >= reviewPages - 1}
                    onClick={() => setReviewPage((p) => Math.min(reviewPages - 1, p + 1))}
                  >
                    {t("settings.autoTag.nextPage")}
                  </button>
                </div>
              )}
            </div>
            {queueQ.isError ? (
              <p className="tgm-empty">{t("settings.autoTag.queueUnavailable")}</p>
            ) : queueQ.isLoading ? (
              <p className="tgm-empty">{t("common.loading")}</p>
            ) : reviewItems.length === 0 ? (
              <p className="tgm-empty">{t("settings.autoTag.reviewEmpty")}</p>
            ) : (
              <div className="tgm-queue-list">
                {reviewItems.map((item) => (
                  <div key={item.id} className="tgm-qrow">
                    <div className="tgm-qrow-main">
                      <span className="tgm-qname">{item.name}</span>
                      <span className="tgm-qcount">
                        {t("settings.autoTag.articleCount", { count: item.articleCount })}
                      </span>
                      {item.suppressed && (
                        <span className="tgm-badge supp">
                          {t("settings.autoTag.suppressedBadge", { n: item.dismissals })}
                        </span>
                      )}
                      {item.parentName && (
                        <span className="tgm-qparent">⇧ {item.parentName}</span>
                      )}
                    </div>
                    {item.samples.length > 0 && (
                      <div className="tgm-qsamples">
                        {item.samples.slice(0, 2).map((s, i) => (
                          <span key={i} className="tgm-qsample">{s}</span>
                        ))}
                      </div>
                    )}
                    <div className="tgm-qrow-actions">
                      <select
                        value={item.tagType ?? ""}
                        title={t("settings.autoTag.typeHint")}
                        onChange={(e) =>
                          queueType(item.id, e.target.value === "" ? null : (e.target.value as "entity" | "topic"))
                        }
                      >
                        <option value="">{t("settings.autoTag.typeNone")}</option>
                        <option value="entity">{t("settings.autoTag.typeEntity")}</option>
                        <option value="topic">{t("settings.autoTag.typeTopic")}</option>
                      </select>
                      <button
                        className="tgm-btn primary"
                        onClick={() => confirmReviewItem(item)}
                      >
                        {t("settings.autoTag.confirm")}
                      </button>
                      <button
                        className="tgm-btn"
                        disabled={item.suppressed}
                        onClick={() => suppressReviewItem(item)}
                      >
                        {t("settings.autoTag.suppress")}
                      </button>
                      <button className="tgm-btn danger" onClick={() => deleteReviewItem(item)}>
                        {t("common.delete")}
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        ) : (
          <div className="tgm-queue">
            <div className="tgm-queue-head">
              <span className="tgm-queue-title">{t("settings.autoTag.suppressedTags")}</span>
              {suppPages > 1 && (
                <div className="tgm-pager">
                  <button
                    disabled={suppPage <= 0}
                    onClick={() => setSuppPage((p) => Math.max(0, p - 1))}
                  >
                    {t("settings.autoTag.prevPage")}
                  </button>
                  <span>
                    {t("settings.autoTag.pageOf", {
                      current: suppPage + 1,
                      total: suppPages,
                    })}
                  </span>
                  <button
                    disabled={suppPage >= suppPages - 1}
                    onClick={() => setSuppPage((p) => Math.min(suppPages - 1, p + 1))}
                  >
                    {t("settings.autoTag.nextPage")}
                  </button>
                </div>
              )}
            </div>
            {suppListQ.isError ? (
              <p className="tgm-empty">{t("settings.autoTag.queueUnavailable")}</p>
            ) : suppListQ.isLoading ? (
              <p className="tgm-empty">{t("common.loading")}</p>
            ) : suppItems.length === 0 ? (
              <p className="tgm-empty">{t("settings.autoTag.suppressedEmpty")}</p>
            ) : (
              <div className="tgm-queue-list">
                {suppItems.map((item) => (
                  <div key={item.id} className="tgm-qrow">
                    <div className="tgm-qrow-main">
                      <span className="tgm-qname">{item.name}</span>
                      <span className="tgm-qcount">
                        {t("settings.autoTag.articleCount", { count: item.articleCount })}
                      </span>
                      <span className="tgm-badge supp">
                        {t("settings.autoTag.dismissedBadge", { n: item.dismissals })}
                      </span>
                      {item.suppressedAt && (
                        <span className="tgm-qdate">
                          {new Date(item.suppressedAt).toLocaleDateString()}
                        </span>
                      )}
                    </div>
                    <div className="tgm-qrow-actions">
                      <button className="tgm-btn primary" onClick={() => restoreItem(item)}>
                        {t("settings.autoTag.restore")}
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function AddTagForm({
  kind,
  topics,
  onCancel,
  onSubmit,
}: {
  kind: TagKind;
  topics: Tag[];
  onCancel: () => void;
  onSubmit: (name: string, type: "entity" | "topic" | null, parent?: string) => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [typeSel, setTypeSel] = useState<"" | "entity" | "topic">("topic");
  const [parent, setParent] = useState("");
  const disabled = !name.trim();
  return (
    <form
      className="tgm-addform"
      onSubmit={(e) => {
        e.preventDefault();
        if (disabled) return;
        onSubmit(name.trim(), (typeSel || null) as "entity" | "topic" | null, parent || undefined);
      }}
    >
      <input autoFocus value={name} onChange={(e) => setName(e.target.value)}
        placeholder={t("tagManage.addName")} />
      {kind === "ai" && (
        <select value={typeSel} onChange={(e) => setTypeSel(e.target.value as "" | "entity" | "topic")}>
          <option value="topic">{t("tagManage.asTopic")}</option>
          <option value="entity">{t("tagManage.asEntity")}</option>
          <option value="">{t("tagManage.noType")}</option>
        </select>
      )}
      {kind === "ai" && (
        <select value={parent} onChange={(e) => setParent(e.target.value)}>
          <option value="">{t("tagManage.noParent")}</option>
          {topics.map((tp) => (
            <option key={tp.id} value={tp.name}>
              ⇧ {tp.name}
            </option>
          ))}
        </select>
      )}
      <button type="submit" disabled={disabled}>{t("tagManage.create")}</button>
      <button type="button" onClick={onCancel}>{t("tagManage.cancel")}</button>
    </form>
  );
}

function AliasEditor({ tagId, invalidate }: { tagId: number; invalidate: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["tag-aliases", tagId],
    queryFn: () => listTagAliases({ tagId }),
    enabled: tagId > 0,
  });
  const err = (e: unknown) =>
    window.alert(String((e as { message?: string })?.message ?? e));
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["tag-aliases"] });
    invalidate();
  };
  const [adding, setAdding] = useState(false);
  const [val, setVal] = useState("");
  const aliases = q.data ?? [];
  return (
    <div className="tgm-aliases">
      <h4>{t("tagManage.aliases")}</h4>
      <ul>
        {aliases.map((a) => (
          <li key={a.id}>
            {a.alias}
            <button
              className="tgm-remove"
              title={t("tagManage.removeAlias")}
              onClick={() => void deleteTagAlias(a.id).then(refresh).catch(err)}
            >
              ✕
            </button>
          </li>
        ))}
      </ul>
      {adding ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (!val.trim()) return;
            void createTagAlias(tagId, val.trim()).then(() => { setVal(""); setAdding(false); refresh(); }).catch(err);
          }}
        >
          <input autoFocus value={val} onChange={(e) => setVal(e.target.value)}
            placeholder={t("tagManage.aliasPlaceholder")} />
          <button type="submit">{t("tagManage.save")}</button>
        </form>
      ) : (
        <button onClick={() => setAdding(true)}>{t("tagManage.addAlias")}</button>
      )}
    </div>
  );
}
