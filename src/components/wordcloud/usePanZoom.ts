import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from "react";

const MIN_SCALE = 0.45;
const MAX_SCALE = 3.5;
const DRAG_THRESHOLD = 5;

type PanZoomView = {
  pan: { x: number; y: number };
  scale: number;
};

/** Pointer drag + wheel zoom for a container. Resets when `resetKey` changes. */
export function usePanZoom(
  containerRef: RefObject<HTMLElement | null>,
  resetKey?: unknown,
) {
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [scale, setScale] = useState(1);
  const [dragging, setDragging] = useState(false);
  const viewRef = useRef<PanZoomView>({ pan: { x: 0, y: 0 }, scale: 1 });
  const dragRef = useRef({
    active: false,
    moved: false,
    startX: 0,
    startY: 0,
    originX: 0,
    originY: 0,
  });
  const suppressClickRef = useRef(false);

  useEffect(() => {
    viewRef.current = { pan: { x: 0, y: 0 }, scale: 1 };
    setPan({ x: 0, y: 0 });
    setScale(1);
  }, [resetKey]);

  useEffect(() => {
    viewRef.current = { pan, scale };
  }, [pan, scale]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = container.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      const { pan: p, scale: s } = viewRef.current;
      const factor = Math.exp(-e.deltaY * 0.002);
      const nextScale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, s * factor));
      const wx = (mx - p.x) / s;
      const wy = (my - p.y) / s;
      const nextPan = {
        x: mx - wx * nextScale,
        y: my - wy * nextScale,
      };
      viewRef.current = { pan: nextPan, scale: nextScale };
      setPan(nextPan);
      setScale(nextScale);
    };

    container.addEventListener("wheel", onWheel, { passive: false });
    return () => container.removeEventListener("wheel", onWheel);
  }, [containerRef]);

  const onPointerDown = useCallback((e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    const { pan: p } = viewRef.current;
    dragRef.current = {
      active: true,
      moved: false,
      startX: e.clientX,
      startY: e.clientY,
      originX: p.x,
      originY: p.y,
    };
    suppressClickRef.current = false;
    setDragging(true);
  }, []);

  const onPointerMove = useCallback((e: ReactPointerEvent) => {
    if (!dragRef.current.active) return;
    const dx = e.clientX - dragRef.current.startX;
    const dy = e.clientY - dragRef.current.startY;
    if (Math.abs(dx) > DRAG_THRESHOLD || Math.abs(dy) > DRAG_THRESHOLD) {
      dragRef.current.moved = true;
      suppressClickRef.current = true;
    }
    if (!dragRef.current.moved) return;
    const nextPan = {
      x: dragRef.current.originX + dx,
      y: dragRef.current.originY + dy,
    };
    viewRef.current = { ...viewRef.current, pan: nextPan };
    setPan(nextPan);
  }, []);

  const onPointerUp = useCallback(() => {
    dragRef.current.active = false;
    setDragging(false);
  }, []);

  const resetView = useCallback(() => {
    viewRef.current = { pan: { x: 0, y: 0 }, scale: 1 };
    setPan({ x: 0, y: 0 });
    setScale(1);
  }, []);

  const shouldSuppressClick = useCallback(() => {
    return suppressClickRef.current || dragRef.current.moved;
  }, []);

  const cursor =
    dragging && dragRef.current.moved ? ("grabbing" as const) : ("grab" as const);

  return {
    pan,
    scale,
    cursor,
    onPointerDown,
    onPointerMove,
    onPointerUp,
    resetView,
    shouldSuppressClick,
  };
}
