// The filter language of the telemetry tab (`mallard-otel-cli/src/tui/query.rs`).

export interface TraceFilter {
  service?: string;
  operation?: string;
  minMs?: number;
  maxMs?: number;
  errorsOnly: boolean;
  attributes: string[];
}

export interface LogFilter {
  service?: string;
  minSeverity?: number;
  trace?: string;
  q?: string;
  attributes: string[];
}

/** The words as typed, with their quotes: splits on spaces outside `"double quotes"`. */
function rawTokens(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quoted = false;
  for (const ch of s) {
    if (ch === " " && !quoted) {
      if (cur) out.push(cur);
      cur = "";
    } else {
      if (ch === '"') quoted = !quoted;
      cur += ch;
    }
  }
  if (cur) out.push(cur);
  return out;
}

const unquote = (t: string) => t.replaceAll('"', "");

/** Splits on spaces, `"double quotes"` keep spaces. */
export function tokens(s: string): string[] {
  return rawTokens(s).map(unquote);
}

/** `250` (ms), `250ms`, `1.5s`, `2m`, `100us`, in ms. */
export function parseDurationMs(s: string): number | null {
  const m = /^(\d+(?:\.\d+)?)(us|µs|ms|s|m)?$/.exec(s.trim());
  if (!m) return null;
  const n = Number(m[1]);
  switch (m[2]) {
    case "us":
    case "µs":
      return n / 1000;
    case "s":
      return n * 1000;
    case "m":
      return n * 60_000;
    default:
      return n;
  }
}

const TRUE = new Set(["1", "true", "yes", "on"]);

function splitPair(t: string): [string, string] | null {
  const i = t.indexOf("=");
  return i > 0 ? [t.slice(0, i), t.slice(i + 1)] : null;
}

/** A bad filter returns an error and the caller keeps the old one, as in the TUI. */
export function parseTraceFilter(s: string): TraceFilter | { error: string } {
  const f: TraceFilter = { errorsOnly: false, attributes: [] };
  for (const t of tokens(s)) {
    const pair = splitPair(t);
    if (!pair) {
      const lower = t.toLowerCase();
      if (["errors", "error", "err"].includes(lower)) {
        f.errorsOnly = true;
        continue;
      }
      if (t.startsWith(">")) {
        const d = parseDurationMs(t.slice(1));
        if (d === null) return { error: `not a duration: ${t.slice(1)}` };
        f.minMs = d;
        continue;
      }
      return { error: `unknown filter "${t}"; use service=, op=, min=, max=, errors or key=value` };
    }
    const [k, v] = pair;
    switch (k.toLowerCase()) {
      case "service":
      case "svc":
      case "s":
        f.service = v;
        break;
      case "op":
      case "operation":
      case "name":
      case "o":
        f.operation = v;
        break;
      case "min":
      case "min_ms":
      case "max":
      case "max_ms": {
        const d = parseDurationMs(v);
        if (d === null) return { error: `not a duration: ${v}` };
        if (k.toLowerCase().startsWith("min")) f.minMs = d;
        else f.maxMs = d;
        break;
      }
      case "errors":
      case "error":
        f.errorsOnly = TRUE.has(v.toLowerCase());
        break;
      default:
        f.attributes.push(`${k}=${v}`);
    }
  }
  return f;
}

export const SEVERITY_NAMES: Record<string, number> = {
  trace: 1, debug: 5, info: 9, warn: 13, warning: 13, error: 17, err: 17, fatal: 21,
};

/** `info`, or 1–24. */
export function parseSeverity(s: string): number | null {
  const named = SEVERITY_NAMES[s.toLowerCase()];
  if (named) return named;
  const n = Number(s);
  return Number.isInteger(n) && n >= 1 && n <= 24 ? n : null;
}

export function parseLogFilter(s: string): LogFilter | { error: string } {
  const f: LogFilter = { attributes: [] };
  const words: string[] = [];
  for (const t of tokens(s)) {
    const pair = splitPair(t);
    if (!pair) {
      words.push(t);
      continue;
    }
    const [k, v] = pair;
    switch (k.toLowerCase()) {
      case "service":
      case "svc":
      case "s":
        f.service = v;
        break;
      case "level":
      case "lvl":
      case "severity":
      case "sev": {
        const n = parseSeverity(v);
        if (n === null) return { error: `unknown level "${v}"; use trace, debug, info, warn, error, fatal or 1-24` };
        f.minSeverity = n;
        break;
      }
      case "trace":
      case "trace_id":
        f.trace = v;
        break;
      default:
        f.attributes.push(`${k}=${v}`);
    }
  }
  if (words.length) f.q = words.join(" ");
  return f;
}

export interface IssueFilter {
  tags: string[];
  query?: string;
}

/** The search box of the issue list: `key=value` is a tag, other words search the
 *  title and culprit. So does a word with its `=` in quotes: `"a=b"` is text, while
 *  `a="b c"` is a tag. The server takes at most 10 tags. */
export function parseIssueFilter(s: string): IssueFilter | { error: string } {
  const tags: string[] = [];
  const words: string[] = [];
  for (const t of rawTokens(s)) {
    const pair = splitPair(t);
    if (pair && !/[":]/.test(pair[0])) tags.push(`${pair[0]}:${unquote(pair[1])}`);
    else words.push(unquote(t));
  }
  if (tags.length > 10) return { error: "at most 10 tag filters" };
  return { tags, query: words.length ? words.join(" ") : undefined };
}

const LEVEL = /(^|\s)(level|lvl|severity|sev)=\S*/gi;

/** `s` on the logs screen: all → info → warn → error → all, rewriting the `level=` token. */
export function cycleLevel(filter: string): string {
  const cur = /(?:^|\s)(?:level|lvl|severity|sev)=(\S*)/i.exec(filter)?.[1];
  const n = cur ? parseSeverity(cur) : null;
  const next = n === null ? "info" : n < 13 ? "warn" : n < 17 ? "error" : "";
  const rest = filter.replace(LEVEL, " ").trim().replace(/\s+/g, " ");
  return next ? `level=${next}${rest ? ` ${rest}` : ""}` : rest;
}

/** Sets a key=value token, replacing an earlier one with that key. */
export function setToken(filter: string, key: string, value: string): string {
  const kept = tokens(filter).filter((t) => !t.toLowerCase().startsWith(`${key.toLowerCase()}=`));
  const quote = (t: string) => (t.includes(" ") ? t.replace(/=(.*)$/, '="$1"') : t);
  return [...kept.map(quote), quote(`${key}=${value}`)].join(" ");
}

/** What `mallard find` can look up: 32 hex (trace, event) or 16 hex (issue, span). */
export function parseFindId(s: string): string | null {
  const hex = s.trim().replaceAll("-", "").toLowerCase();
  return /^[0-9a-f]+$/.test(hex) && (hex.length === 32 || hex.length === 16) ? hex : null;
}

/** A trace id in any form; null when it is not one. */
export function parseTraceId(s: string): string | null {
  const hex = s.trim().replaceAll("-", "").toLowerCase();
  return /^[0-9a-f]{32}$/.test(hex) ? hex : null;
}
