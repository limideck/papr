import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import {
  asWordCloudGroup,
  GROUP_COLORS,
  WORD_CLOUD_GROUPS,
  type WordCloudGroup,
} from "../lib/wordcloud";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import type {
  WordCloudEntitiesSource,
  WordCloudEntity,
} from "../types";
import { reportError } from "../toast";

type SubTab = "stopwords" | "entities";

const WORDCLOUD_STATUS_KEY = ["wordcloud-status"] as const;

/** Short all-lowercase residuals are usually acronyms (ai → AI). */
function suggestCanonicalFromResidual(raw: string): string {
  const t = raw.trim();
  if (/^[a-z]{2,3}$/.test(t)) return t.toUpperCase();
  return t;
}

export default function WordCloudConfigAdmin() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<SubTab>("stopwords");

  return (
    <div className="wc-admin">
      <p className="settings-group-desc">{t("settings.wordcloud.desc")}</p>

      <TermIndexBackfill />

      <div className="wc-admin-tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={tab === "stopwords"}
          className={tab === "stopwords" ? "active" : ""}
          onClick={() => setTab("stopwords")}
        >
          {t("settings.wordcloud.tabStopwords")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "entities"}
          className={tab === "entities" ? "active" : ""}
          onClick={() => setTab("entities")}
        >
          {t("settings.wordcloud.tabEntities")}
        </button>
      </div>

      {tab === "stopwords" ? <StopwordsReadonly /> : <EntitiesEditor />}
    </div>
  );
}

function TermIndexBackfill() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [running, setRunning] = useState(false);
  const statusQuery = useQuery({
    queryKey: WORDCLOUD_STATUS_KEY,
    queryFn: () => api.getWordCloudStatus(),
  });

  useEffect(() => {
    if (statusQuery.error) reportError(statusQuery.error);
  }, [statusQuery.error]);

  const indexed = statusQuery.data?.indexed ?? 0;
  const stale = statusQuery.data?.stale ?? 0;
  const missing = statusQuery.data?.missing ?? 0;
  const total = statusQuery.data?.totalArticles ?? 0;
  const dictVersion = statusQuery.data?.dictVersion ?? 0;
  const loading = statusQuery.isLoading || statusQuery.isFetching;
  const remaining = stale + missing;

  const runBatch = async () => {
    setRunning(true);
    try {
      // Drain in a few sync batches so the admin sees progress without waiting
      // solely on the background worker.
      let left = remaining;
      let guard = 0;
      while (left > 0 && guard < 40) {
        const res = await api.backfillWordCloud({ sync: true, limit: 200 });
        qc.setQueryData(WORDCLOUD_STATUS_KEY, {
          indexed: res.indexed,
          stale: res.stale,
          missing: res.missing,
          totalArticles: res.totalArticles,
          dictVersion: res.dictVersion,
        });
        left = res.remaining ?? res.stale + res.missing;
        if ((res.processed ?? 0) === 0) break;
        guard += 1;
      }
    } catch (e) {
      reportError(e);
    } finally {
      setRunning(false);
      void qc.invalidateQueries({ queryKey: WORDCLOUD_STATUS_KEY });
    }
  };

  return (
    <div className="wc-admin-panel" style={{ marginBottom: "1.25rem" }}>
      <h3 className="settings-group-title">{t("settings.wordcloud.backfillTitle")}</h3>
      <p className="settings-group-desc">{t("settings.wordcloud.backfillDesc")}</p>
      {loading && !statusQuery.data ? (
        <p className="settings-group-desc">{t("common.loading")}</p>
      ) : (
        <p className="settings-group-desc">
          {t("settings.wordcloud.backfillStatus", {
            indexed,
            remaining,
            total,
            version: dictVersion,
          })}
        </p>
      )}
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", paddingTop: 8 }}>
        <button
          type="button"
          className="s-btn"
          disabled={running || loading}
          onClick={() => void qc.invalidateQueries({ queryKey: WORDCLOUD_STATUS_KEY })}
        >
          {t("settings.wordcloud.backfillRefresh")}
        </button>
        <button
          type="button"
          className="s-btn primary"
          disabled={running || loading || remaining === 0}
          onClick={() => void runBatch()}
        >
          {running
            ? t("settings.wordcloud.backfillRunning")
            : t("settings.wordcloud.backfillRun")}
        </button>
      </div>
    </div>
  );
}

function StopwordsReadonly() {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [version, setVersion] = useState(1);
  const [words, setWords] = useState<string[]>([]);
  const [query, setQuery] = useState("");

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api
      .getWordCloudStopwords()
      .then((data) => {
        if (cancelled) return;
        setVersion(data.version ?? 1);
        setWords([...(data.words ?? [])].sort((a, b) => a.localeCompare(b, "zh-CN")));
      })
      .catch((e) => {
        reportError(e);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return words;
    return words.filter((w) => w.toLowerCase().includes(q));
  }, [words, query]);

  if (loading) {
    return <p className="settings-group-desc">{t("common.loading")}</p>;
  }

  return (
    <div className="wc-admin-panel">
      <p className="settings-group-desc">
        {t("settings.wordcloud.stopwordsHint", {
          count: words.length,
          version,
        })}
      </p>
      <div className="wc-admin-search">
        <input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("settings.wordcloud.searchStopwords")}
        />
        <span>{t("settings.wordcloud.wordCount", { count: filtered.length })}</span>
      </div>
      <div className="wc-admin-chips">
        {filtered.map((w) => (
          <span key={w} className="wc-admin-chip">
            {w}
          </span>
        ))}
        {filtered.length === 0 && (
          <p className="settings-group-desc">{t("settings.wordcloud.noMatch")}</p>
        )}
      </div>
    </div>
  );
}

function sourceLabelKey(source: WordCloudEntitiesSource | undefined): string {
  switch (source) {
    case "local":
      return "settings.wordcloud.sourceLocal";
    case "explicit":
      return "settings.wordcloud.sourceExplicit";
    default:
      return "settings.wordcloud.sourceShared";
  }
}

function EntitiesEditor() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [loading, setLoading] = useState(true);
  const [version, setVersion] = useState(1);
  const [entities, setEntities] = useState<WordCloudEntity[]>([]);
  const [source, setSource] = useState<WordCloudEntitiesSource | undefined>();
  const [path, setPath] = useState<string | undefined>();
  const [cowDir, setCowDir] = useState<string | undefined>();
  const [groupFilter, setGroupFilter] = useState<WordCloudGroup | "all">("all");
  const [query, setQuery] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draftCanonical, setDraftCanonical] = useState("");
  const [draftAliases, setDraftAliases] = useState("");
  const [saving, setSaving] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [addCanonical, setAddCanonical] = useState("");
  const [addAliases, setAddAliases] = useState("");
  const [addGroup, setAddGroup] = useState<WordCloudGroup>("general");
  const [addId, setAddId] = useState("");
  const [creating, setCreating] = useState(false);

  const load = () => {
    setLoading(true);
    return api
      .getWordCloudEntities()
      .then((data) => {
        setVersion(data.version ?? 1);
        setEntities(data.entities ?? []);
        setSource(data.source);
        setPath(data.path);
        setCowDir(data.cowDir);
      })
      .catch((e) => {
        reportError(e);
      })
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api
      .getWordCloudEntities()
      .then((data) => {
        if (cancelled) return;
        setVersion(data.version ?? 1);
        setEntities(data.entities ?? []);
        setSource(data.source);
        setPath(data.path);
        setCowDir(data.cowDir);
      })
      .catch((e) => {
        reportError(e);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entities.filter((e) => {
      if (groupFilter !== "all" && asWordCloudGroup(e.group) !== groupFilter) {
        return false;
      }
      if (!q) return true;
      const hay = `${e.canonical} ${e.id} ${(e.aliases ?? []).join(" ")}`.toLowerCase();
      return hay.includes(q);
    });
  }, [entities, groupFilter, query]);

  const startEdit = (e: WordCloudEntity) => {
    setEditingId(e.id);
    setDraftCanonical(e.canonical);
    setDraftAliases((e.aliases ?? []).join("\n"));
  };

  const cancelEdit = () => {
    setEditingId(null);
    setDraftCanonical("");
    setDraftAliases("");
  };

  const applyMeta = (res: {
    version: number;
    source?: WordCloudEntitiesSource;
    path?: string;
    cowDir?: string;
  }) => {
    setVersion(res.version);
    if (res.source) setSource(res.source);
    if (res.path) setPath(res.path);
    if (res.cowDir) setCowDir(res.cowDir);
    void qc.invalidateQueries({ queryKey: ["wordcloud-entities"] });
    void qc.invalidateQueries({ queryKey: WORDCLOUD_STATUS_KEY });
  };

  const saveEdit = async () => {
    if (!editingId) return;
    const canonical = draftCanonical.trim();
    if (!canonical) return;
    const aliases = draftAliases
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter(Boolean);
    setSaving(true);
    try {
      const res = await api.patchWordCloudEntity(editingId, {
        canonical,
        aliases,
      });
      setEntities((prev) =>
        prev.map((e) => (e.id === editingId ? res.entity : e)),
      );
      applyMeta(res);
      cancelEdit();
    } catch (e) {
      reportError(e);
    } finally {
      setSaving(false);
    }
  };

  const resetAdd = () => {
    setShowAdd(false);
    setAddCanonical("");
    setAddAliases("");
    setAddGroup("general");
    setAddId("");
  };

  const saveCreate = async () => {
    const canonical = addCanonical.trim();
    if (!canonical) return;
    const aliases = addAliases
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter(Boolean);
    setCreating(true);
    try {
      const res = await api.createWordCloudEntity({
        canonical,
        group: addGroup,
        aliases,
        id: addId.trim() || undefined,
      });
      setEntities((prev) => [...prev, res.entity].sort((a, b) =>
        a.canonical.localeCompare(b.canonical, undefined, { sensitivity: "base" }),
      ));
      applyMeta(res);
      resetAdd();
      setQuery(res.entity.canonical);
    } catch (e) {
      reportError(e);
    } finally {
      setCreating(false);
    }
  };

  if (loading) {
    return <p className="settings-group-desc">{t("common.loading")}</p>;
  }

  return (
    <div className="wc-admin-panel">
      <p className="settings-group-desc">
        {t("settings.wordcloud.entitiesHint", {
          count: entities.length,
          version,
        })}
      </p>
      <p className="settings-group-desc">{t("settings.wordcloud.residualHint")}</p>
      <p className="settings-group-desc wc-admin-source">
        <span className={`wc-admin-source-badge source-${source ?? "shared"}`}>
          {t(sourceLabelKey(source))}
        </span>
        {path ? (
          <span className="wc-admin-path" title={path}>
            {path}
          </span>
        ) : null}
      </p>
      {source === "shared" && (
        <p className="settings-group-desc">
          {t("settings.wordcloud.cowHint", {
            dir: cowDir || "wordcloud",
          })}
        </p>
      )}
      <div className="wc-admin-filters">
        <select
          value={groupFilter}
          onChange={(e) =>
            setGroupFilter(e.target.value as WordCloudGroup | "all")
          }
        >
          <option value="all">{t("settings.wordcloud.allGroups")}</option>
          {WORD_CLOUD_GROUPS.map((g) => (
            <option key={g} value={g}>
              {t(`wordcloud.group.${g}`)}
            </option>
          ))}
        </select>
        <input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("settings.wordcloud.searchEntities")}
          {...NO_AUTOCORRECT}
        />
        <button type="button" className="s-btn" onClick={() => void load()}>
          {t("settings.wordcloud.backfillRefresh")}
        </button>
        <button
          type="button"
          className="s-btn primary"
          disabled={editingId !== null || showAdd}
          onClick={() => {
            setShowAdd(true);
            // Prefill from search box when promoting a residual like "ai".
            const q = query.trim();
            if (q && !addCanonical) {
              const canonical = suggestCanonicalFromResidual(q);
              setAddCanonical(canonical);
              // Keep lowercase residual as alias when we upcased for acronyms.
              setAddAliases(
                canonical !== q && q.toLowerCase() === q ? q : "",
              );
            }
          }}
        >
          {t("settings.wordcloud.addEntity")}
        </button>
      </div>
      {showAdd && (
        <div className="wc-admin-add">
          <p className="settings-group-desc">{t("settings.wordcloud.addEntityHint")}</p>
          <div className="wc-admin-add-row">
            <label>
              {t("settings.wordcloud.colCanonical")}
              <input
                className="wc-admin-edit-input"
                value={addCanonical}
                onChange={(e) => setAddCanonical(e.target.value)}
                placeholder="AI"
                autoFocus
                {...NO_AUTOCORRECT}
              />
            </label>
            <label>
              {t("settings.wordcloud.colGroup")}
              <select
                value={addGroup}
                onChange={(e) => setAddGroup(e.target.value as WordCloudGroup)}
              >
                {WORD_CLOUD_GROUPS.map((g) => (
                  <option key={g} value={g}>
                    {t(`wordcloud.group.${g}`)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {t("settings.wordcloud.colId")}
              <input
                className="wc-admin-edit-input"
                value={addId}
                onChange={(e) => setAddId(e.target.value)}
                placeholder={t("settings.wordcloud.idPlaceholder")}
                {...NO_AUTOCORRECT}
              />
            </label>
          </div>
          <label className="wc-admin-add-aliases">
            {t("settings.wordcloud.colAliases")}
            <textarea
              className="wc-admin-edit-aliases"
              value={addAliases}
              onChange={(e) => setAddAliases(e.target.value)}
              rows={2}
              placeholder={t("settings.wordcloud.aliasesPlaceholder")}
              {...NO_AUTOCORRECT}
            />
          </label>
          <div className="wc-admin-actions">
            <button
              type="button"
              className="s-btn primary"
              disabled={creating || !addCanonical.trim()}
              onClick={() => void saveCreate()}
            >
              {creating
                ? t("settings.wordcloud.saving")
                : t("settings.wordcloud.create")}
            </button>
            <button
              type="button"
              className="s-btn"
              disabled={creating}
              onClick={resetAdd}
            >
              {t("settings.wordcloud.cancel")}
            </button>
          </div>
        </div>
      )}
      <div className="wc-admin-table-wrap">
        <table className="wc-admin-table">
          <thead>
            <tr>
              <th>{t("settings.wordcloud.colCanonical")}</th>
              <th>{t("settings.wordcloud.colGroup")}</th>
              <th>{t("settings.wordcloud.colAliases")}</th>
              <th>{t("settings.wordcloud.colActions")}</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((e) => {
              const g = asWordCloudGroup(e.group);
              const isEditing = editingId === e.id;
              return (
                <tr key={e.id} className={isEditing ? "wc-admin-row-edit" : undefined}>
                  <td>
                    {isEditing ? (
                      <input
                        className="wc-admin-edit-input"
                        value={draftCanonical}
                        onChange={(ev) => setDraftCanonical(ev.target.value)}
                        aria-label={t("settings.wordcloud.colCanonical")}
                        autoFocus
                        {...NO_AUTOCORRECT}
                      />
                    ) : (
                      <strong>{e.canonical}</strong>
                    )}
                    <div className="wc-admin-id">{e.id}</div>
                  </td>
                  <td>
                    <span className="wc-admin-group">
                      <span
                        className="wordcloud-legend-dot"
                        style={{ backgroundColor: GROUP_COLORS[g] }}
                      />
                      {t(`wordcloud.group.${g}`)}
                    </span>
                  </td>
                  <td className="wc-admin-aliases">
                    {isEditing ? (
                      <textarea
                        className="wc-admin-edit-aliases"
                        value={draftAliases}
                        onChange={(ev) => setDraftAliases(ev.target.value)}
                        rows={3}
                        aria-label={t("settings.wordcloud.colAliases")}
                        placeholder={t("settings.wordcloud.aliasesPlaceholder")}
                        {...NO_AUTOCORRECT}
                      />
                    ) : (
                      <>
                        {(e.aliases ?? []).slice(0, 12).join(" · ")}
                        {(e.aliases?.length ?? 0) > 12
                          ? ` · +${(e.aliases?.length ?? 0) - 12}`
                          : ""}
                      </>
                    )}
                  </td>
                  <td className="wc-admin-actions">
                    {isEditing ? (
                      <>
                        <button
                          type="button"
                          className="s-btn primary"
                          disabled={saving || !draftCanonical.trim()}
                          onClick={() => void saveEdit()}
                        >
                          {saving
                            ? t("settings.wordcloud.saving")
                            : t("settings.wordcloud.save")}
                        </button>
                        <button
                          type="button"
                          className="s-btn"
                          disabled={saving}
                          onClick={cancelEdit}
                        >
                          {t("settings.wordcloud.cancel")}
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className="s-btn"
                        disabled={editingId !== null || showAdd}
                        onClick={() => startEdit(e)}
                      >
                        {t("settings.wordcloud.edit")}
                      </button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {filtered.length === 0 && (
          <p className="settings-group-desc">{t("settings.wordcloud.noMatch")}</p>
        )}
      </div>
      <p className="settings-group-desc">{t("settings.wordcloud.editBackfillHint")}</p>
    </div>
  );
}
