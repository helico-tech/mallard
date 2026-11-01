// The parts of a Sentry event payload the screens show
// (`mallard-sentry-cli/src/event_view.rs`). Everything is optional in the protocol.

type Obj = Record<string, unknown>;
const asObj = (v: unknown): Obj | undefined => (v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : undefined);
const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const num = (v: unknown): number | undefined => (typeof v === "number" && v >= 0 ? v : undefined);

export interface Frame {
  function?: string;
  module?: string;
  filename?: string;
  lineno?: number;
  colno?: number;
  inApp: boolean;
  contextLine?: string;
  preContext: string[];
  postContext: string[];
}

export const functionName = (f: Frame): string => (f.function && f.function !== "?" ? f.function : "<anonymous>");

export function location(f: Frame): string {
  let s = f.filename ?? f.module ?? "<unknown>";
  if (f.lineno !== undefined) {
    s += `:${f.lineno}`;
    if (f.colno !== undefined) s += `:${f.colno}`;
  }
  return s;
}

export function context(f: Frame): { line?: number; text: string; error: boolean }[] {
  if (f.contextLine === undefined) return [];
  const n = f.lineno;
  const pre = f.preContext.length;
  return [
    ...f.preContext.map((text, i) => ({ line: n === undefined ? undefined : Math.max(0, n - (pre - i)), text, error: false })),
    { line: n, text: f.contextLine, error: true },
    ...f.postContext.map((text, i) => ({ line: n === undefined ? undefined : n + 1 + i, text, error: false })),
  ];
}

function toFrame(v: unknown): Frame {
  const o = asObj(v) ?? {};
  const lines = (k: string) => (Array.isArray(o[k]) ? (o[k] as unknown[]).map((l) => str(l) ?? "") : []);
  return {
    function: str(o.function),
    module: str(o.module),
    filename: str(o.filename) ?? str(o.abs_path),
    lineno: num(o.lineno),
    colno: num(o.colno),
    inApp: o.in_app === true,
    contextLine: str(o.context_line),
    preContext: lines("pre_context"),
    postContext: lines("post_context"),
  };
}

const framesOf = (st: unknown): Frame[] => {
  const f = asObj(st)?.frames;
  return Array.isArray(f) ? f.map(toFrame) : [];
};

const values = (v: unknown): unknown[] => {
  if (Array.isArray(v)) return v;
  const inner = asObj(v)?.values;
  return Array.isArray(inner) ? inner : [];
};

export interface Exception {
  type: string;
  value?: string;
  mechanism?: string;
  handled?: boolean;
  /** Oldest call first, as sent. */
  frames: Frame[];
  rawFrames?: Frame[];
}

/** Primary (outermost) first. Without an exception, a thread or top-level
 *  stack trace becomes one pseudo-exception. */
export function exceptions(payload: unknown): Exception[] {
  const p = asObj(payload) ?? {};
  const out: Exception[] = values(p.exception).map((e) => {
    const o = asObj(e) ?? {};
    const mech = asObj(o.mechanism);
    return {
      type: str(o.type) ?? "Error",
      value: str(o.value),
      mechanism: str(mech?.type),
      handled: typeof mech?.handled === "boolean" ? mech.handled : undefined,
      frames: framesOf(o.stacktrace),
      rawFrames: o.raw_stacktrace ? framesOf(o.raw_stacktrace) : undefined,
    };
  });
  out.reverse();
  if (out.length === 0) {
    const threads = values(p.threads).map((t) => asObj(t) ?? {});
    const thread = threads.find((t) => t.crashed === true && t.stacktrace) ?? threads.find((t) => t.stacktrace);
    const frames = framesOf(thread?.stacktrace ?? p.stacktrace);
    if (frames.length) {
      out.push({
        type: "Stack trace",
        value: message(p),
        frames,
        rawFrames: thread?.raw_stacktrace ? framesOf(thread.raw_stacktrace) : undefined,
      });
    }
  }
  return out;
}

export function message(payload: unknown): string | undefined {
  const p = asObj(payload) ?? {};
  const le = asObj(p.logentry);
  const m = le?.formatted ?? le?.message ?? p.message;
  const text = typeof m === "string" ? m : str(asObj(m)?.formatted) ?? str(asObj(m)?.message);
  return text || undefined;
}

export function framesNewestFirst(e: Exception, raw: boolean): Frame[] {
  return [...(raw && e.rawFrames ? e.rawFrames : e.frames)].reverse();
}

export function mechanismLabel(e: Exception): string | undefined {
  if (!e.mechanism) return undefined;
  const h = e.handled === false ? " · unhandled" : e.handled === true ? " · handled" : "";
  return e.mechanism + h;
}

export interface Breadcrumb {
  timestamp?: Date;
  category?: string;
  level: string;
  text: string;
}

const display = (v: unknown): string => (typeof v === "string" ? v : JSON.stringify(v));

function crumbText(o: Obj): string {
  const m = str(o.message);
  if (m !== undefined) return m;
  const d = o.data;
  const data = asObj(d);
  if (data) {
    if (data.method !== undefined && data.url !== undefined) {
      const code = data.status_code !== undefined ? ` -> ${display(data.status_code)}` : "";
      return `${display(data.method)} ${display(data.url)}${code}`;
    }
    return Object.entries(data).map(([k, v]) => `${k}=${display(v)}`).join(" ");
  }
  return d === undefined || d === null ? "" : display(d);
}

function parseTs(v: unknown): Date | undefined {
  if (typeof v === "number") return new Date(v * 1000);
  if (typeof v === "string") {
    const n = Number(v);
    const d = Number.isNaN(n) ? new Date(v) : new Date(n * 1000);
    return Number.isNaN(d.getTime()) ? undefined : d;
  }
  return undefined;
}

/** Newest first. */
export function breadcrumbs(payload: unknown): Breadcrumb[] {
  const out = values(asObj(payload)?.breadcrumbs).map((b) => {
    const o = asObj(b) ?? {};
    return {
      timestamp: parseTs(o.timestamp),
      category: str(o.category) ?? str(o.type),
      level: str(o.level) ?? "info",
      text: crumbText(o),
    };
  });
  return out.reverse();
}

/** runtime, browser, os, device, when known. */
export function contexts(payload: unknown): [string, string][] {
  const c = asObj(asObj(payload)?.contexts);
  if (!c) return [];
  const out: [string, string][] = [];
  for (const key of ["runtime", "browser", "os", "device"]) {
    const ctx = asObj(c[key]);
    if (!ctx) continue;
    const name = str(ctx.name);
    const version = str(ctx.version);
    const text = name ? (version ? `${name} ${version}` : name) : str(ctx.model) ?? str(ctx.arch);
    if (text) out.push([key, text]);
  }
  return out;
}

export function request(payload: unknown): string | undefined {
  const r = asObj(asObj(payload)?.request);
  if (!r) return undefined;
  const parts = [str(r.method), str(r.url)].filter(Boolean);
  return parts.length ? parts.join(" ") : undefined;
}
