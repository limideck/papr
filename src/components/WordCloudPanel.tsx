import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import type { WordCloudTerm } from "../types";

type DaysPreset = 1 | 3 | 7;

interface Props {
  onSelectTerm: (term: string) => void;
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

  const useCustom = days === null;

  useEffect(() => {
    const ctrl = new AbortController();
    if (useCustom && (!from || !to)) return;

    setLoading(true);
    setError(null);
    const params = useCustom
      ? { from, to }
      : { days: days ?? 1 };

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

  const max = terms[0]?.count ?? 1;
  const min = terms[terms.length - 1]?.count ?? 1;

  const fontSize = (count: number) => {
    if (max <= min) return 13;
    const ratio = (count - min) / (max - min);
    return 10 + ratio * 10;
  };

  return (
    <div className="wordcloud">
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

      <div className="wordcloud-body">
        {loading && <div className="wordcloud-status">{t("common.loading")}</div>}
        {!loading && error && (
          <div className="wordcloud-status error">{error}</div>
        )}
        {!loading && !error && terms.length === 0 && (
          <div className="wordcloud-status">{t("wordcloud.empty")}</div>
        )}
        {!loading &&
          !error &&
          terms.map((item) => (
            <button
              key={item.term}
              type="button"
              className="wordcloud-term"
              style={{ fontSize: fontSize(item.count) }}
              title={`${item.term} (${item.count})`}
              onClick={() => onSelectTerm(item.term)}
            >
              {item.term}
            </button>
          ))}
      </div>

      {!loading && scanned > 0 && (
        <div className="wordcloud-meta">
          {t("wordcloud.scanned", { count: scanned })}
        </div>
      )}
    </div>
  );
}
