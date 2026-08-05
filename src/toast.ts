// Global toast notifications.
//
// A single transient pill at the bottom of the window, driven by a tiny
// store so any module — React component or not — can raise one without
// threading a callback through props.
//
// Three flavours:
//   • show   — a brief confirmation ("Starred", "Marked 12 read")
//   • error  — a failure: styled distinctly, held far longer, dismissible
//   • undo   — a confirmation carrying an Undo action (see `withUndo`)

import { create } from "zustand";
import i18n from "./i18n";
import { errorText } from "./lib/errors";

export type ToastTone = "default" | "error";

export interface ToastAction {
  label: string;
  run: () => void;
}

export interface ToastItem {
  id: number;
  text: string;
  kbd?: string;
  tone: ToastTone;
  action?: ToastAction;
  /** ms on screen before auto-dismiss. */
  duration: number;
}

interface ToastState {
  current: ToastItem | null;
  push: (t: Omit<ToastItem, "id">) => number;
  dismiss: (id?: number) => void;
}

// A monotonic counter, not Date.now(): two toasts raised in the same
// millisecond would otherwise collide on the React render key.
let seq = 0;

export const useToasts = create<ToastState>((set, get) => ({
  current: null,
  push: (t) => {
    const id = ++seq;
    set({ current: { ...t, id } });
    return id;
  },
  // `dismiss()` clears whatever is showing; `dismiss(id)` only clears that
  // toast, so a stale timer can't wipe a newer one that has since replaced it.
  dismiss: (id) => {
    const cur = get().current;
    if (cur && (id === undefined || cur.id === id)) set({ current: null });
  },
}));

// A confirmation reads at a glance; an error must survive a glance away; an
// undo window must be long enough to actually catch a misclick.
const SUCCESS_MS = 1900;
const ERROR_MS = 7000;
const UNDO_MS = 6000;

export const toast = {
  show: (text: string, kbd?: string) =>
    useToasts
      .getState()
      .push({ text, kbd, tone: "default", duration: SUCCESS_MS }),
  error: (text: string) =>
    useToasts.getState().push({ text, tone: "error", duration: ERROR_MS }),
};

/** Raise an error toast from any caught exception. */
export function reportError(e: unknown): void {
  toast.error(errorText(e));
}

/**
 * Run a destructive action behind a grace period, with an Undo toast.
 *
 * `apply` runs now so the UI reacts immediately (the row vanishes). `commit`
 * — the irreversible backend call — is deferred until the window closes;
 * `revert` runs instead if the user clicks Undo. Nothing is destroyed on the
 * server until the window elapses, so Undo needs no soft-delete support.
 */
export function withUndo(opts: {
  text: string;
  apply: () => void;
  commit: () => void;
  revert: () => void;
}): void {
  opts.apply();
  let settled = false;
  const timer = window.setTimeout(() => {
    if (settled) return;
    settled = true;
    opts.commit();
  }, UNDO_MS);
  useToasts.getState().push({
    text: opts.text,
    tone: "default",
    duration: UNDO_MS,
    action: {
      label: i18n.t("common.undo"),
      run: () => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timer);
        opts.revert();
      },
    },
  });
}
