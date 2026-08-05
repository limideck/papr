import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import type { DailyCount } from "../types";

const DAYS = 30;

function isoDate(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** ISO week number (Mon-based), matching the reference “W28” labels. */
function isoWeek(d: Date): number {
  const t = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()));
  const dayNum = t.getUTCDay() || 7;
  t.setUTCDate(t.getUTCDate() + 4 - dayNum);
  const yearStart = new Date(Date.UTC(t.getUTCFullYear(), 0, 1));
  return Math.ceil(((t.getTime() - yearStart.getTime()) / 86400000 + 1) / 7);
}

type Cell = { date: string; count: number } | null;

type WeekRow = { key: string; label: string; cells: Cell[] };

function buildWeeks(counts: Map<string, number>): WeekRow[] {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const rangeStart = new Date(today);
  rangeStart.setDate(rangeStart.getDate() - (DAYS - 1));

  // Align grid to Monday of the week that contains rangeStart.
  const mondayOffset = (rangeStart.getDay() + 6) % 7;
  const cursor = new Date(rangeStart);
  cursor.setDate(cursor.getDate() - mondayOffset);

  const weeks: WeekRow[] = [];
  while (cursor <= today) {
    const weekStart = new Date(cursor);
    const cells: Cell[] = [];
    for (let i = 0; i < 7; i++) {
      const key = isoDate(cursor);
      if (cursor < rangeStart || cursor > today) {
        cells.push(null);
      } else {
        cells.push({ date: key, count: counts.get(key) ?? 0 });
      }
      cursor.setDate(cursor.getDate() + 1);
    }
    weeks.push({
      key: isoDate(weekStart),
      label: `W${isoWeek(weekStart)}`,
      cells,
    });
  }
  return weeks;
}

/** GitHub-style green levels from relative intensity. */
function level(count: number, max: number): number {
  if (count <= 0 || max <= 0) return 0;
  const r = count / max;
  if (r <= 0.25) return 1;
  if (r <= 0.5) return 2;
  if (r <= 0.75) return 3;
  return 4;
}

export default function ActivityHeatmap() {
  const { t } = useTranslation();
  const { data } = useQuery({
    queryKey: ["dailyCounts", DAYS],
    queryFn: () => api.dailyArticleCounts(DAYS),
  });

  const [tip, setTip] = useState<{
    x: number;
    y: number;
    date: string;
    count: number;
  } | null>(null);

  const { weeks, total, max } = useMemo(() => {
    const map = new Map<string, number>();
    let sum = 0;
    let m = 0;
    for (const row of data ?? ([] as DailyCount[])) {
      map.set(row.date, row.count);
      sum += row.count;
      if (row.count > m) m = row.count;
    }
    return { weeks: buildWeeks(map), total: sum, max: m };
  }, [data]);

  const weekdayKeys = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] as const;

  return (
    <div className="activity-heatmap">
      <div className="activity-heatmap-summary">
        <div className="activity-heatmap-label">{t("sidebar.last30Days")}</div>
        <div className="activity-heatmap-total">{total.toLocaleString()}</div>
      </div>
      <div className="activity-heatmap-chart">
        <div className="activity-heatmap-rows">
          {weeks.map((week) => (
            <div key={week.key} className="activity-heatmap-row">
              <span className="activity-heatmap-week">{week.label}</span>
              <div className="activity-heatmap-cells">
                {week.cells.map((cell, i) =>
                  cell == null ? (
                    <span
                      key={`${week.key}-e${i}`}
                      className="activity-heatmap-cell empty"
                    />
                  ) : (
                    <span
                      key={cell.date}
                      className={`activity-heatmap-cell l${level(cell.count, max)}`}
                      onMouseEnter={(e) => {
                        const r = (e.target as HTMLElement).getBoundingClientRect();
                        setTip({
                          x: r.left + r.width / 2,
                          y: r.top,
                          date: cell.date,
                          count: cell.count,
                        });
                      }}
                      onMouseLeave={() => setTip(null)}
                    />
                  ),
                )}
              </div>
            </div>
          ))}
        </div>
        <div className="activity-heatmap-weekdays">
          <span className="activity-heatmap-week spacer" aria-hidden />
          <div className="activity-heatmap-cells">
            {weekdayKeys.map((k) => (
              <span key={k} className="activity-heatmap-wd">
                {t(`sidebar.weekday.${k}`)}
              </span>
            ))}
          </div>
        </div>
      </div>
      {tip && (
        <div
          className="activity-heatmap-tip"
          style={{ left: tip.x, top: tip.y }}
          role="tooltip"
        >
          <div>{tip.date}</div>
          <div>{tip.count}</div>
        </div>
      )}
    </div>
  );
}
