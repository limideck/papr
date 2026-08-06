import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  asWordCloudGroup,
  GROUP_COLORS,
} from "../../lib/wordcloud";
import type { WordCloudTerm } from "../../types";

interface Props {
  terms: WordCloudTerm[];
  onWordClick?: (word: string, additive: boolean) => void;
}

const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));
const MAX_WORDS = 56;

type Point3 = { x: number; y: number; z: number };

type LaidOut = {
  term: string;
  color: string;
  fontSize: number;
  pos: Point3;
};

function fibonacci(index: number, total: number, radius: number): Point3 {
  const y = 1 - (index / Math.max(total - 1, 1)) * 2;
  const r = Math.sqrt(Math.max(0, 1 - y * y));
  const theta = GOLDEN_ANGLE * index;
  return {
    x: Math.cos(theta) * r * radius,
    y: y * radius,
    z: Math.sin(theta) * r * radius,
  };
}

function rotateY(p: Point3, a: number): Point3 {
  const c = Math.cos(a);
  const s = Math.sin(a);
  return { x: p.x * c + p.z * s, y: p.y, z: -p.x * s + p.z * c };
}

function rotateX(p: Point3, a: number): Point3 {
  const c = Math.cos(a);
  const s = Math.sin(a);
  return { x: p.x, y: p.y * c - p.z * s, z: p.y * s + p.z * c };
}

export default function WordCloudGlobe({ terms, onWordClick }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [rot, setRot] = useState({ x: 0.25, y: 0 });
  const [zoom, setZoom] = useState(1);
  const drag = useRef({
    active: false,
    moved: false,
    sx: 0,
    sy: 0,
    ox: 0,
    oy: 0,
  });
  const rotRef = useRef(rot);
  rotRef.current = rot;

  const layout = useMemo(() => {
    const sorted = [...terms]
      .sort((a, b) => b.count - a.count)
      .slice(0, MAX_WORDS);
    if (sorted.length === 0) return [] as LaidOut[];
    const maxW = Math.max(...sorted.map((t) => t.count));
    const minW = Math.min(...sorted.map((t) => t.count));
    return sorted.map((t, i) => {
      const ratio = maxW > minW ? (t.count - minW) / (maxW - minW) : 1;
      return {
        term: t.term,
        color: GROUP_COLORS[asWordCloudGroup(t.group)] ?? GROUP_COLORS.general,
        fontSize: 11 + ratio * 14,
        pos: fibonacci(i, sorted.length, 1),
      };
    });
  }, [terms]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      setSize({
        w: Math.floor(entry.contentRect.width),
        h: Math.floor(entry.contentRect.height),
      });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Gentle auto-rotate when idle.
  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = (now - last) / 1000;
      last = now;
      if (!drag.current.active) {
        setRot((r) => ({ ...r, y: r.y + dt * 0.35 }));
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      setZoom((z) =>
        Math.min(2.2, Math.max(0.55, z * Math.exp(-e.deltaY * 0.002))),
      );
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const radius = Math.min(size.w, size.h) * 0.36 * zoom;
  const cx = size.w / 2;
  const cy = size.h / 2;

  const projected = useMemo(() => {
    return layout
      .map((item) => {
        let p = rotateY(item.pos, rot.y);
        p = rotateX(p, rot.x);
        const depth = (p.z + 1) / 2;
        const scale = 0.55 + depth * 0.7;
        return {
          ...item,
          sx: cx + p.x * radius,
          sy: cy + p.y * radius,
          depth,
          scale,
          fontSize: item.fontSize * scale,
          opacity: 0.35 + depth * 0.65,
        };
      })
      .sort((a, b) => a.depth - b.depth);
  }, [layout, rot, radius, cx, cy]);

  const onPointerDown = useCallback((e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    drag.current = {
      active: true,
      moved: false,
      sx: e.clientX,
      sy: e.clientY,
      ox: rotRef.current.x,
      oy: rotRef.current.y,
    };
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }, []);

  const onPointerMove = useCallback((e: ReactPointerEvent) => {
    if (!drag.current.active) return;
    const dx = e.clientX - drag.current.sx;
    const dy = e.clientY - drag.current.sy;
    if (Math.abs(dx) > 4 || Math.abs(dy) > 4) drag.current.moved = true;
    setRot({
      x: drag.current.ox + dy * 0.008,
      y: drag.current.oy + dx * 0.01,
    });
  }, []);

  const onPointerUp = useCallback(() => {
    drag.current.active = false;
  }, []);

  const handleClick = (term: string, additive: boolean) => {
    if (drag.current.moved) return;
    onWordClick?.(term, additive);
  };

  return (
    <div
      ref={containerRef}
      className="wc-view wc-globe"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onDoubleClick={() => {
        setRot({ x: 0.25, y: 0 });
        setZoom(1);
      }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {projected.map((w) => (
        <button
          key={w.term}
          type="button"
          className="wc-globe-word"
          style={{
            left: w.sx,
            top: w.sy,
            fontSize: w.fontSize,
            color: w.color,
            opacity: w.opacity,
            zIndex: Math.round(w.depth * 100),
          }}
          title={w.term}
          onClick={(e) => handleClick(w.term, e.shiftKey || e.metaKey)}
        >
          {w.term}
        </button>
      ))}
    </div>
  );
}
