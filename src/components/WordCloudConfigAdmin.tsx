import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import {
  asWordCloudGroup,
  GROUP_COLORS,
  WORD_CLOUD_GROUPS,
  type WordCloudGroup,
} from "../lib/wordcloud";
import type { WordCloudEntity } from "../types";
import { reportError } from "../toast";

type SubTab = "stopwords" | "entities";

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

      {tab === "stopwords" ? <StopwordsReadonly /> : <EntitiesReadonly />}
    </div>
  );
}

function TermIndexBackfill() {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);
  const [indexed, setIndexed] = useState(0);
  const [stale, setStale] = useState(0);
  const [missing, setMissing] = useState(0);
  const [total, setTotal] = useState(0);
  const [dictVersion, setDictVersion] = useState(0);

  const refresh = () => {
    setLoading(true);
    api
      .getWordCloudStatus()
      .then((st) => {
        setIndexed(st.indexed);
        setStale(st.stale);
        setMissing(st.missing);
        setTotal(st.totalArticles);
        setDictVersion(st.dictVersion);
      })
      .catch((e) => reportError(e))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    refresh();
  }, [t]);

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
        setIndexed(res.indexed);
        setStale(res.stale);
        setMissing(res.missing);
        setTotal(res.totalArticles);
        setDictVersion(res.dictVersion);
        left = res.remaining ?? res.stale + res.missing;
        if ((res.processed ?? 0) === 0) break;
        guard += 1;
      }
    } catch (e) {
      reportError(e);
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="wc-admin-panel" style={{ marginBottom: "1.25rem" }}>
      <h3 className="settings-group-title">{t("settings.wordcloud.backfillTitle")}</h3>
      <p className="settings-group-desc">{t("settings.wordcloud.backfillDesc")}</p>
      {loading ? (
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
        <button type="button" className="s-btn" disabled={running || loading} onClick={refresh}>
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

function EntitiesReadonly() {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [version, setVersion] = useState(1);
  const [entities, setEntities] = useState<WordCloudEntity[]>([]);
  const [groupFilter, setGroupFilter] = useState<WordCloudGroup | "all">("all");
  const [query, setQuery] = useState("");

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api
      .getWordCloudEntities()
      .then((data) => {
        if (cancelled) return;
        setVersion(data.version ?? 1);
        setEntities(data.entities ?? []);
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
        />
      </div>
      <div className="wc-admin-table-wrap">
        <table className="wc-admin-table">
          <thead>
            <tr>
              <th>{t("settings.wordcloud.colCanonical")}</th>
              <th>{t("settings.wordcloud.colGroup")}</th>
              <th>{t("settings.wordcloud.colAliases")}</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((e) => {
              const g = asWordCloudGroup(e.group);
              return (
                <tr key={e.id}>
                  <td>
                    <strong>{e.canonical}</strong>
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
                    {(e.aliases ?? []).slice(0, 12).join(" · ")}
                    {(e.aliases?.length ?? 0) > 12
                      ? ` · +${(e.aliases?.length ?? 0) - 12}`
                      : ""}
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
    </div>
  );
}
