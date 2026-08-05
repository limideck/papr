// Shared fetch helpers for the web API client.
//
// All requests go to same-origin `/api/...` (Vite proxies to papr-server in
// dev). Session auth is cookie-based (`credentials: "include"`). A 401 clears
// the session and surfaces the login screen via `onUnauthorized`.

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly detail: string | null;

  constructor(status: number, code: string, detail?: string | null) {
    super(detail || code);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.detail = detail ?? null;
  }
}

type UnauthorizedHandler = () => void;

let onUnauthorized: UnauthorizedHandler | null = null;

/** Register the callback invoked when any API call returns 401. */
export function setUnauthorizedHandler(handler: UnauthorizedHandler | null): void {
  onUnauthorized = handler;
}

async function parseError(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let detail: string | null = null;
  try {
    const body = (await res.json()) as {
      code?: string;
      error?: string;
      detail?: string | null;
      message?: string;
    };
    if (typeof body.code === "string") code = body.code;
    detail =
      (typeof body.detail === "string" && body.detail) ||
      (typeof body.error === "string" && body.error) ||
      (typeof body.message === "string" && body.message) ||
      null;
  } catch {
    /* non-JSON body */
  }
  return new ApiError(res.status, code, detail);
}

/** Low-level fetch with credentials + 401 handling. */
export async function apiFetch(
  path: string,
  init: RequestInit = {},
): Promise<Response> {
  const headers = new Headers(init.headers);
  if (
    init.body != null &&
    typeof init.body === "string" &&
    !headers.has("Content-Type")
  ) {
    headers.set("Content-Type", "application/json");
  }
  const res = await fetch(path, {
    ...init,
    headers,
    credentials: "include",
  });
  if (res.status === 401) {
    onUnauthorized?.();
  }
  return res;
}

/** JSON request that throws `ApiError` on non-2xx. */
export async function apiJson<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const res = await apiFetch(path, init);
  if (!res.ok) throw await parseError(res);
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

/** Binary request (e.g. proxied images). */
export async function apiBytes(
  path: string,
  init: RequestInit = {},
): Promise<ArrayBuffer> {
  const res = await apiFetch(path, init);
  if (!res.ok) throw await parseError(res);
  return res.arrayBuffer();
}

/** POST/GET that streams SSE (or NDJSON) events until the response ends. */
export async function apiStream(
  path: string,
  init: RequestInit,
  onEvent: (data: unknown) => void,
): Promise<void> {
  const res = await apiFetch(path, init);
  if (!res.ok) throw await parseError(res);
  if (!res.body) return;

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  const flushLine = (line: string) => {
    const trimmed = line.trimEnd();
    if (!trimmed || trimmed.startsWith(":")) return;
    let payload = trimmed;
    if (trimmed.startsWith("data:")) {
      payload = trimmed.slice(5).trimStart();
    }
    if (!payload || payload === "[DONE]") return;
    try {
      onEvent(JSON.parse(payload));
    } catch {
      /* ignore malformed chunks */
    }
  };

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const parts = buffer.split(/\r?\n/);
    buffer = parts.pop() ?? "";
    for (const line of parts) flushLine(line);
  }
  if (buffer.trim()) flushLine(buffer);
}

/** Build a query string from a record, dropping null/undefined. */
export function qs(
  params: Record<string, string | number | boolean | null | undefined>,
): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === null || v === undefined) continue;
    sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}
