import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  asWordCloudGroup,
  GROUP_COLORS,
} from "../../lib/wordcloud";
import type { WordCloudTerm } from "../../types";
import { usePanZoom } from "./usePanZoom";

interface Props {
  terms: WordCloudTerm[];
  onWordClick?: (word: string, additive: boolean) => void;
}

const TREEMAP_TOP_N = 36;
const MIN_WEIGHT_RATIO = 0.025;
const MIN_LABEL_W = 28;
const MIN_LABEL_H = 18;
const MIN_WEIGHT_W = 52;
const MIN_WEIGHT_H = 36;

type Tile = {
  name: string;
  value: number;
  fill: string;
  x: number;
  y: number;
  w: number;
  h: number;
};

/** Squarified treemap (Bruls et al.) over a unit rectangle, then scaled. */
function layoutTreemap(
  items: { name: string; value: number; fill: string }[],
  width: number,
  height: number,
): Tile[] {
  if (items.length === 0 || width <= 0 || height <= 0) return [];
  const total = items.reduce((s, i) => s + i.value, 0);
  if (total <= 0) return [];

  type Node = { name: string; value: number; fill: string };
  const nodes: Node[] = items.map((i) => ({ ...i }));
  const out: Tile[] = [];

  const worst = (row: Node[], length: number, areaScale: number) => {
    if (row.length === 0) return Infinity;
    const areas = row.map((r) => r.value * areaScale);
    const sum = areas.reduce((a, b) => a + b, 0);
    const max = Math.max(...areas);
    const min = Math.min(...areas);
    return Math.max(
      (length * length * max) / (sum * sum),
      (sum * sum) / (length * length * min),
    );
  };

  const layoutRow = (
    row: Node[],
    x: number,
    y: number,
    w: number,
    h: number,
    horizontal: boolean,
    areaScale: number,
  ) => {
    const sum = row.reduce((s, r) => s + r.value, 0) * areaScale;
    if (horizontal) {
      const rowH = sum / w;
      let cx = x;
      for (const r of row) {
        const rw = (r.value * areaScale) / rowH;
        out.push({ name: r.name, value: r.value, fill: r.fill, x: cx, y, w: rw, h: rowH });
        cx += rw;
      }
      return { x, y: y + rowH, w, h: h - rowH };
    }
    const rowW = sum / h;
    let cy = y;
    for (const r of row) {
      const rh = (r.value * areaScale) / rowW;
      out.push({ name: r.name, value: r.value, fill: r.fill, x, y: cy, w: rowW, h: rh });
      cy += rh;
    }
    return { x: x + rowW, y, w: w - rowW, h };
  };

  let x = 0;
  let y = 0;
  let w = width;
  let h = height;
  const areaScale = (width * height) / total;
  let row: Node[] = [];
  const remaining = [...nodes];

  while (remaining.length > 0) {
    const horizontal = w >= h;
    const length = horizontal ? w : h;
    const next = remaining[0]!;
    const withNext = [...row, next];
    if (
      row.length === 0 ||
      worst(withNext, length, areaScale) <= worst(row, length, areaScale)
    ) {
      row = withNext;
      remaining.shift();
    } else {
      const box = layoutRow(row, x, y, w, h, horizontal, areaScale);
      x = box.x;
      y = box.y;
      w = box.w;
      h = box.h;
      row = [];
    }
  }
  if (row.length > 0) {
    layoutRow(row, x, y, w, h, w >= h, areaScale);
  }
  return out;
}

export default function WordCloudTreemap({ terms, onWordClick }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const {
    pan,
    scale,
    cursor,
    onPointerDown,
    onPointerMove,
    onPointerUp,
    resetView,
    shouldSuppressClick,
  } = usePanZoom(containerRef, terms);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const ro = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      setSize({ w: Math.floor(width), h: Math.floor(height) });
    });
    ro.observe(container);
    return () => ro.disconnect();
  }, []);

  const tiles = useMemo(() => {
    const sorted = [...terms].sort((a, b) => b.count - a.count);
    if (sorted.length === 0 || size.w <= 0 || size.h <= 0) return [] as Tile[];
    const max = sorted[0]!.count;
    const filtered = sorted
      .filter((t) => t.count >= max * MIN_WEIGHT_RATIO)
      .slice(0, TREEMAP_TOP_N)
      .map((t) => ({
        name: t.term,
        value: t.count,
        fill: GROUP_COLORS[asWordCloudGroup(t.group)] ?? GROUP_COLORS.general,
      }));
    // Layout into the measured panel rect so the SVG fills without letterboxing.
    return layoutTreemap(filtered, size.w, size.h);
  }, [terms, size.w, size.h]);

  const handleClick = useCallback(
    (name: string, additive: boolean) => {
      if (shouldSuppressClick()) return;
      onWordClick?.(name, additive);
    },
    [onWordClick, shouldSuppressClick],
  );

  return (
    <div
      ref={containerRef}
      className="wc-view wc-treemap"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onPointerLeave={onPointerUp}
      onDoubleClick={resetView}
      onContextMenu={(e) => e.preventDefault()}
      style={{ cursor }}
    >
      <div
        className="wc-treemap-stage"
        style={{
          transform: `translate(${pan.x}px, ${pan.y}px) scale(${scale})`,
          transformOrigin: "0 0",
        }}
      >
        {size.w > 0 && size.h > 0 && (
          <svg
            viewBox={`0 0 ${size.w} ${size.h}`}
            className="wc-treemap-svg"
            preserveAspectRatio="none"
          >
            {tiles.map((tile) => {
              const pad = 1.5;
              const innerW = Math.max(0, tile.w - pad * 2);
              const innerH = Math.max(0, tile.h - pad * 2);
              const showLabel = innerW >= MIN_LABEL_W && innerH >= MIN_LABEL_H;
              const showWeight =
                showLabel && innerW >= MIN_WEIGHT_W && innerH >= MIN_WEIGHT_H;
              const fontSize = Math.max(
                10,
                Math.min(
                  16,
                  Math.min(innerW / Math.max(1, tile.name.length * 0.62), innerH * 0.42),
                ),
              );
              return (
                <g
                  key={tile.name}
                  style={{ cursor: "pointer" }}
                  onClick={(e) =>
                    handleClick(tile.name, e.shiftKey || e.metaKey)
                  }
                  onContextMenu={(e) => e.preventDefault()}
                >
                  <title>
                    {tile.name} · {tile.value}
                  </title>
                  <rect
                    x={tile.x + pad}
                    y={tile.y + pad}
                    width={innerW}
                    height={innerH}
                    rx={3}
                    ry={3}
                    fill={tile.fill}
                    stroke="#fff"
                    strokeWidth={1}
                    opacity={0.9}
                  />
                  {showLabel && (
                    <text
                      x={tile.x + tile.w / 2}
                      y={tile.y + tile.h / 2 - (showWeight ? 6 : 0)}
                      textAnchor="middle"
                      dominantBaseline="middle"
                      fill="#fff"
                      fontSize={fontSize}
                      fontWeight={600}
                      style={{ pointerEvents: "none", userSelect: "none" }}
                    >
                      {tile.name}
                    </text>
                  )}
                  {showWeight && (
                    <text
                      x={tile.x + tile.w / 2}
                      y={tile.y + tile.h / 2 + fontSize * 0.75}
                      textAnchor="middle"
                      dominantBaseline="middle"
                      fill="rgba(255,255,255,0.85)"
                      fontSize={Math.max(8, fontSize * 0.7)}
                      style={{ pointerEvents: "none", userSelect: "none" }}
                    >
                      {tile.value}
                    </text>
                  )}
                </g>
              );
            })}
          </svg>
        )}
      </div>
    </div>
  );
}
