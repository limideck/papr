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

/** Must match rendered `.wc-flat-word` / `--ui` — canvas cannot resolve CSS variables. */
const FONT_FAMILY =
  "'Inter Tight Variable', 'Inter Tight', system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
const MAX_WORDS = 48;
const COLLISION_PAD = 6;
const ARC_STEP_PX = 10;
/** Extra width fudge so measured boxes stay slightly larger than glyphs. */
const WIDTH_FUDGE = 1.08;

type PlacedWord = {
  text: string;
  x: number;
  y: number;
  fontSize: number;
  color: string;
  boxW: number;
  boxH: number;
};

let measureCanvas: HTMLCanvasElement | null = null;

function measureText(text: string, fontSize: number): { boxW: number; boxH: number } {
  // Match `.wc-flat-word` padding: 2px 5px, line-height 1.2
  const padX = 12;
  const padY = 8;
  if (!measureCanvas) measureCanvas = document.createElement("canvas");
  const ctx = measureCanvas.getContext("2d");
  if (!ctx) {
    return {
      boxW: text.length * fontSize * 0.62 * WIDTH_FUDGE + padX,
      boxH: fontSize * 1.2 + padY,
    };
  }
  ctx.font = `600 ${fontSize}px ${FONT_FAMILY}`;
  const metrics = ctx.measureText(text);
  return {
    boxW: metrics.width * WIDTH_FUDGE + padX,
    boxH: fontSize * 1.2 + padY,
  };
}

function boxesOverlap(
  ax: number,
  ay: number,
  aw: number,
  ah: number,
  bx: number,
  by: number,
  bw: number,
  bh: number,
  pad: number,
): boolean {
  return !(
    ax + aw / 2 + pad < bx - bw / 2 ||
    ax - aw / 2 - pad > bx + bw / 2 ||
    ay + ah / 2 + pad < by - bh / 2 ||
    ay - ah / 2 - pad > by + bh / 2
  );
}

function fitsInBounds(
  x: number,
  y: number,
  boxW: number,
  boxH: number,
  width: number,
  height: number,
  margin: number,
): boolean {
  return (
    x - boxW / 2 >= margin &&
    y - boxH / 2 >= margin &&
    x + boxW / 2 <= width - margin &&
    y + boxH / 2 <= height - margin
  );
}

/**
 * Archimedean spiral stretched to the panel ellipse so words fill width and
 * height instead of clustering in a vertically squashed disc.
 */
function layoutWords(
  terms: WordCloudTerm[],
  width: number,
  height: number,
): PlacedWord[] {
  if (terms.length === 0 || width <= 0 || height <= 0) return [];

  const sorted = [...terms].sort((a, b) => b.count - a.count).slice(0, MAX_WORDS);
  const maxW = Math.max(...sorted.map((x) => x.count));
  const minW = Math.min(...sorted.map((x) => x.count));
  const cx = width / 2;
  const cy = height / 2;
  const margin = 8;
  const rx = Math.max(1, width / 2 - margin);
  const ry = Math.max(1, height / 2 - margin);
  const placed: PlacedWord[] = [];

  // Scale type slightly with panel area so larger panels don't look sparse-center.
  const areaScale = Math.sqrt((width * height) / (360 * 420));
  const minFont = 10;
  const maxFont = Math.round(Math.min(36, Math.max(22, 22 * areaScale)));

  for (let i = 0; i < sorted.length; i++) {
    const item = sorted[i]!;
    const t = maxW > minW ? (item.count - minW) / (maxW - minW) : 1;
    const targetFont = Math.round(minFont + t * (maxFont - minFont));
    const color =
      GROUP_COLORS[asWordCloudGroup(item.group)] ?? GROUP_COLORS.general;

    let found = false;
    let x = cx;
    let y = cy;
    let fontSize = targetFont;
    let boxW = 0;
    let boxH = 0;

    // Prefer target size; shrink only if no free slot exists.
    for (let size = targetFont; size >= minFont && !found; size -= 2) {
      fontSize = size;
      ({ boxW, boxH } = measureText(item.term, fontSize));
      if (boxW > width - margin * 2 || boxH > height - margin * 2) continue;

      const tStep = Math.max(
        0.01,
        (Math.min(boxW, boxH) * 0.45) / Math.max(rx, ry),
      );
      const phase = (i % 7) * 0.37;

      for (let u = 0; u <= 1.2 && !found; u += tStep) {
        const steps =
          u < 1e-6
            ? 1
            : Math.max(
                14,
                Math.ceil(
                  (2 * Math.PI * Math.hypot(u * rx, u * ry)) / ARC_STEP_PX,
                ),
              );
        for (let s = 0; s < steps; s++) {
          const theta = phase + (s / steps) * Math.PI * 2;
          x = cx + Math.cos(theta) * u * rx;
          y = cy + Math.sin(theta) * u * ry;

          if (!fitsInBounds(x, y, boxW, boxH, width, height, margin)) continue;

          const hits = placed.some((p) =>
            boxesOverlap(
              x,
              y,
              boxW,
              boxH,
              p.x,
              p.y,
              p.boxW,
              p.boxH,
              COLLISION_PAD,
            ),
          );
          if (!hits) {
            found = true;
            break;
          }
        }
      }
    }

    if (!found) continue;
    placed.push({ text: item.term, x, y, fontSize, color, boxW, boxH });
  }

  return placed;
}

export default function WordCloudFlat({ terms, onWordClick }: Props) {
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

  const placed = useMemo(
    () => layoutWords(terms, size.w, size.h),
    [terms, size.w, size.h],
  );

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

  const handleWordClick = useCallback(
    (text: string, additive: boolean) => {
      if (shouldSuppressClick()) return;
      onWordClick?.(text, additive);
    },
    [onWordClick, shouldSuppressClick],
  );

  return (
    <div
      ref={containerRef}
      className="wc-view wc-flat"
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
        className="wc-flat-stage"
        style={{
          transform: `translate(${pan.x}px, ${pan.y}px) scale(${scale})`,
          transformOrigin: "0 0",
        }}
      >
        {placed.map((word) => (
          <button
            key={word.text}
            type="button"
            className="wc-flat-word"
            onClick={(e) => handleWordClick(word.text, e.shiftKey || e.metaKey)}
            onContextMenu={(e) => e.preventDefault()}
            style={{
              left: word.x,
              top: word.y,
              fontSize: word.fontSize,
              color: word.color,
            }}
            title={`${word.text}`}
          >
            {word.text}
          </button>
        ))}
      </div>
    </div>
  );
}
