import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import {
  asWordCloudGroup,
  GROUP_COLORS,
  WORD_CLOUD_GROUPS,
  type WordCloudGroup,
} from "../lib/wordcloud";
import type { WordCloudTerm } from "../types";
import Icon from "./Icon";
import WordCloudFlat from "./wordcloud/WordCloudFlat";
import WordCloudGlobe from "./wordcloud/WordCloudGlobe";
import WordCloudTreemap from "./wordcloud/WordCloudTreemap";

type DaysPreset = 1 | 3 | 7;
type ViewMode = "flat" | "globe" | "treemap";

interface Props {
  onSelectTerm: (term: string, additive?: boolean) => void;
}

function todayISO(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

function daysAgoISO(n: number): string {
  const d = new Date();
  d.setDate(d.getDate() - (n - 1));
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

export default function WordCloudPanel({ onSelectTerm }: Props) {
  const { t } = useTranslation();
  const [days, setDays] = useState<DaysPreset | null>(1);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [terms, setTerms] = useState<WordCloudTerm[]>([]);
  const [scanned, setScanned] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<ViewMode>("flat");
  const [groupFilter, setGroupFilter] = useState<WordCloudGroup | null>(null);

  const useCustom = days === null;

  useEffect(() => {
    const ctrl = new AbortController();
    if (useCustom && (!from || !to)) return;

    setLoading(true);
    setError(null);
    const params = useCustom ? { from, to } : { days: days ?? 1 };

    api
      .getWordCloud(params)
      .then((data) => {
        if (ctrl.signal.aborted) return;
        setTerms(data.terms ?? []);
        setScanned(data.scanned ?? 0);
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return;
        if (err instanceof DOMException && err.name === "AbortError") return;
        setError(err instanceof Error ? err.message : t("wordcloud.loadError"));
        setTerms([]);
      })
      .finally(() => {
        if (!ctrl.signal.aborted) setLoading(false);
      });

    return () => ctrl.abort();
  }, [days, from, to, useCustom, t]);

  const visible = useMemo(() => {
    if (!groupFilter) return terms;
    return terms.filter((item) => asWordCloudGroup(item.group) === groupFilter);
  }, [terms, groupFilter]);

  const toggleGroup = (g: WordCloudGroup) => {
    setGroupFilter((cur) => (cur === g ? null : g));
  };

  const hintKey =
    view === "globe"
      ? "wordcloud.hintGlobe"
      : view === "treemap"
        ? "wordcloud.hintTreemap"
        : "wordcloud.hintFlat";

  return (
    <div className="wordcloud">
      <div className="wordcloud-toolbar">
        <div className="wordcloud-presets">
          {([1, 3, 7] as DaysPreset[]).map((d) => (
            <button
              key={d}
              type="button"
              className={days === d ? "active" : ""}
              onClick={() => {
                setDays(d);
                setFrom("");
                setTo("");
              }}
            >
              {t(`wordcloud.days${d}`)}
            </button>
          ))}
          <button
            type="button"
            className={useCustom ? "active" : ""}
            onClick={() => {
              setDays(null);
              if (!from) setFrom(daysAgoISO(7));
              if (!to) setTo(todayISO());
            }}
          >
            {t("wordcloud.custom")}
          </button>
        </div>

        <div className="wordcloud-modes" role="group" aria-label={t("wordcloud.modes")}>
          <button
            type="button"
            className={view === "flat" ? "active" : ""}
            title={t("wordcloud.modeFlat")}
            onClick={() => setView("flat")}
          >
            <Icon name="grid" size={12} />
            <span>{t("wordcloud.modeFlat")}</span>
          </button>
          <button
            type="button"
            className={view === "globe" ? "active" : ""}
            title={t("wordcloud.modeGlobe")}
            onClick={() => setView("globe")}
          >
            <Icon name="globe" size={12} />
            <span>{t("wordcloud.modeGlobe")}</span>
          </button>
          <button
            type="button"
            className={view === "treemap" ? "active" : ""}
            title={t("wordcloud.modeTreemap")}
            onClick={() => setView("treemap")}
          >
            <Icon name="list" size={12} />
            <span>{t("wordcloud.modeTreemap")}</span>
          </button>
        </div>
      </div>

      {useCustom && (
        <div className="wordcloud-range">
          <input
            type="date"
            value={from}
            onChange={(e) => setFrom(e.target.value)}
            aria-label={t("wordcloud.from")}
          />
          <span>–</span>
          <input
            type="date"
            value={to}
            onChange={(e) => setTo(e.target.value)}
            aria-label={t("wordcloud.to")}
          />
        </div>
      )}

      <div className="wordcloud-canvas">
        {loading && <div className="wordcloud-status">{t("common.loading")}</div>}
        {!loading && error && (
          <div className="wordcloud-status error">{error}</div>
        )}
        {!loading && !error && visible.length === 0 && (
          <div className="wordcloud-status">{t("wordcloud.empty")}</div>
        )}
        {!loading && !error && visible.length > 0 && view === "flat" && (
          <WordCloudFlat terms={visible} onWordClick={onSelectTerm} />
        )}
        {!loading && !error && visible.length > 0 && view === "globe" && (
          <WordCloudGlobe terms={visible} onWordClick={onSelectTerm} />
        )}
        {!loading && !error && visible.length > 0 && view === "treemap" && (
          <WordCloudTreemap terms={visible} onWordClick={onSelectTerm} />
        )}
      </div>

      <div className="wordcloud-footer">
        <div className="wordcloud-legend">
          {WORD_CLOUD_GROUPS.map((g) => (
            <button
              key={g}
              type="button"
              className={`wordcloud-legend-item ${groupFilter === g ? "active" : ""} ${
                groupFilter && groupFilter !== g ? "dim" : ""
              }`}
              onClick={() => toggleGroup(g)}
              title={t("wordcloud.filterGroup", { group: t(`wordcloud.group.${g}`) })}
            >
              <span
                className="wordcloud-legend-dot"
                style={{ backgroundColor: GROUP_COLORS[g] }}
              />
              {t(`wordcloud.group.${g}`)}
            </button>
          ))}
        </div>
        <p className="wordcloud-hint">{t(hintKey)}</p>
        {!loading && scanned > 0 && (
          <div className="wordcloud-meta">
            {t("wordcloud.scanned", { count: scanned })}
            {groupFilter &&
              ` · ${t("wordcloud.filtered", {
                group: t(`wordcloud.group.${groupFilter}`),
                count: visible.length,
              })}`}
          </div>
        )}
      </div>
    </div>
  );
}
