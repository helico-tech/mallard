// The one way to the server. Same-origin through mallard web, which holds the
// token. Every string in a response is cleaned here, so no screen can forget.
import { cleanDeep } from "@/lib/clean";

export type ErrorKind = "usage" | "auth" | "not_found" | "conflict" | "unavailable" | "disabled" | "internal";

/** Same mapping as the CLI (ADR 0008). */
function kindOf(status: number, code: string): ErrorKind {
  if (status === 400 || status === 422) return "usage";
  if (status === 401) return "auth";
  if (status === 403) return code === "disabled" ? "disabled" : "auth";
  if (status === 404) return "not_found";
  if (status === 409) return "conflict";
  if (status === 501) return "disabled";
  if ([429, 502, 503, 504].includes(status)) return "unavailable";
  return "internal";
}

const HINTS: Record<ErrorKind, string | null> = {
  usage: "Check the filter or the time window.",
  auth: "Check the API token.",
  not_found: "Check the id, or widen the time window.",
  conflict: null,
  unavailable: "The server is busy or down; it retries by itself.",
  disabled: "This is turned off on the server.",
  internal: "See the server log.",
};

export class ApiError extends Error {
  readonly kind: ErrorKind;
  readonly status: number;
  readonly code: string;
  readonly hint: string | null;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.kind = kindOf(status, code);
    this.status = status;
    this.code = code;
    this.hint = HINTS[this.kind];
  }
}

export type Params = Record<string, string | number | boolean | null | undefined>;

export function qs(params: Params = {}): string {
  const out = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null && v !== "") out.set(k, String(v));
  }
  const s = out.toString();
  return s ? `?${s}` : "";
}

let onUnauthorized: () => void = () => {};
export function setUnauthorizedHandler(fn: () => void): void {
  onUnauthorized = fn;
}

async function failure(res: Response): Promise<ApiError> {
  try {
    const body = cleanDeep((await res.json()) as { error?: string; message?: string });
    return new ApiError(res.status, body.error ?? "error", body.message ?? `the server answered ${res.status}`);
  } catch {
    return new ApiError(res.status, "error", `the server answered ${res.status}`);
  }
}

export interface Page<T> {
  data: T;
  nextCursor: string | null;
}

export async function getPage<T>(path: string, params?: Params, signal?: AbortSignal): Promise<Page<T>> {
  let res: Response;
  try {
    res = await fetch(path + qs(params), { signal, credentials: "same-origin" });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new ApiError(503, "unavailable", "cannot reach mallard web");
  }
  if (!res.ok) {
    const err = await failure(res);
    if (res.status === 401) onUnauthorized();
    throw err;
  }
  return { data: cleanDeep((await res.json()) as T), nextCursor: res.headers.get("x-next-cursor") };
}

export async function get<T>(path: string, params?: Params, signal?: AbortSignal): Promise<T> {
  return (await getPage<T>(path, params, signal)).data;
}

/** Writes carry the header that cross-site requests cannot set (ADR 0016). */
export async function send<T>(method: "PATCH" | "POST", path: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-mallard-web": "1" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(503, "unavailable", "cannot reach mallard web");
  }
  if (!res.ok) {
    const err = await failure(res);
    if (res.status === 401 && !path.startsWith("/web/")) onUnauthorized();
    throw err;
  }
  return (res.status === 204 ? undefined : cleanDeep(await res.json())) as T;
}

export interface Session {
  mode: "token" | "login";
  authenticated: boolean;
  server_url: string | null;
  version: string;
}
