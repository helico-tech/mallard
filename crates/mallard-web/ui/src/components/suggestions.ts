// Where a filter bar's suggestions come from. Keys and values the language knows are
// answered at once; the rest asks the server, a moment after typing stops. Typing never
// waits: a value query can take half a second on a big lake.
import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { otel } from "@/api/otel";
import { sentry } from "@/api/sentry";
import { windowSeconds } from "@/lib/domain";
import { timestamp } from "@/lib/format";
import { quoteValue, type Suggestion, type Token } from "@/lib/suggest";

const DEBOUNCE_MS = 200;
const LIMIT = 30;

export interface Remote {
  items: Suggestion[];
  /** The rows counted start here; set when that cuts the window short. */
  countedFrom?: string;
}

export interface SuggestSource {
  /** Part of the cache key: what the answers depend on, such as app and window. */
  scope: unknown[];
  local: (t: Token) => Suggestion[];
  /** null when the server has nothing to add for this token. */
  remote: (t: Token, signal: AbortSignal) => Promise<Remote> | null;
}

export function useSuggestions(source: SuggestSource | undefined, token: Token, open: boolean) {
  const [settled, setSettled] = useState(token);
  useEffect(() => {
    const id = setTimeout(() => setSettled(token), DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [token]);
  const q = useQuery({
    queryKey: ["suggest", ...(source?.scope ?? []), settled.key, settled.text],
    enabled: !!source && open,
    staleTime: 30_000,
    retry: false,
    queryFn: ({ signal }) => source!.remote(settled, signal) ?? Promise.resolve<Remote>({ items: [] }),
  });
  const local = open && source ? source.local(token) : [];
  const current = settled.key === token.key && settled.text === token.text;
  const seen = new Set(local.map((s) => s.insert));
  const remote = current && q.data ? q.data.items.filter((s) => !seen.has(s.insert)) : [];
  return {
    items: [...local, ...remote],
    loading: open && !!source && (!current || q.isPending),
    failed: open && current && q.isError,
    countedFrom: open && current ? (q.data?.countedFrom ?? null) : null,
  };
}

const startsWith = (s: string, prefix: string) => s.toLowerCase().startsWith(prefix.toLowerCase());

function matching(keys: [string, string][], t: Token): Suggestion[] {
  return keys.filter(([k]) => startsWith(k, t.text)).map(([k, hint]) => ({ label: k, insert: k, hint }));
}

/** Only says so when the rows counted start after the window does. */
function cut(countedFrom: string, w: string): string | undefined {
  return Date.parse(countedFrom) > Date.now() - windowSeconds(w) * 1000 + 60_000 ? timestamp(countedFrom) : undefined;
}

function pair(key: string, value: string, count?: number): Suggestion {
  return { label: value, insert: `${key}=${quoteValue(value)}`, count };
}

function attributes(signal_: "spans" | "logs", t: Token, w: string, service: string[] | undefined, signal: AbortSignal): Promise<Remote> {
  const q = { signal: signal_, since: w, service, prefix: t.text || undefined, limit: LIMIT };
  if (t.key) {
    const key = t.key;
    return otel.attributes({ ...q, key }, signal).then((r) => ({
      countedFrom: cut(r.counted_from, w),
      items: r.items.map((i) => pair(key, i.value ?? "", i.count)),
    }));
  }
  return otel.attributes(q, signal).then((r) => ({
    countedFrom: cut(r.counted_from, w),
    items: r.items.map((i) => ({ label: i.key, insert: `${i.key}=`, count: i.count })),
  }));
}

const SERVICE_KEYS = new Set(["service", "svc", "s"]);
const OP_KEYS = new Set(["op", "operation", "name", "o"]);
const LEVELS = ["trace", "debug", "info", "warn", "error", "fatal"];
const NO_VALUES = new Set(["min", "min_ms", "max", "max_ms", "errors", "error", "trace", "trace_id"]);

/** Operations are read per service and window: 24h is plenty to suggest from. */
const opWindow = (w: string) => (windowSeconds(w) > 86400 ? "24h" : w);

export function traceSuggestions(w: string, service: string[] | undefined, known: string[]): SuggestSource {
  return {
    scope: ["traces", w, service],
    local: (t) => {
      if (t.key === null) {
        return matching([["service=", "service"], ["op=", "operation"], ["min=", "slower than"], ["max=", "faster than"], ["errors", "only with errors"]], t);
      }
      if (SERVICE_KEYS.has(t.key.toLowerCase())) return known.filter((s) => startsWith(s, t.text)).slice(0, LIMIT).map((s) => pair(t.key!, s));
      return [];
    },
    remote: (t, signal) => {
      if (t.key === null) return attributes("spans", t, w, service, signal);
      const key = t.key.toLowerCase();
      if (SERVICE_KEYS.has(key) || NO_VALUES.has(key)) return null;
      if (OP_KEYS.has(key)) {
        return otel.operations(opWindow(w), service, signal).then((ops) => ({
          items: [...new Map(ops.filter((o) => startsWith(o.name, t.text)).map((o) => [o.name, o])).values()]
            .slice(0, LIMIT)
            .map((o) => pair(t.key!, o.name, o.span_count)),
        }));
      }
      return attributes("spans", t, w, service, signal);
    },
  };
}

export function logSuggestions(w: string, service: string[] | undefined, known: string[]): SuggestSource {
  return {
    scope: ["logs", w, service],
    local: (t) => {
      if (t.key === null) return matching([["service=", "service"], ["level=", "at least this severe"], ["trace=", "one trace"]], t);
      const key = t.key.toLowerCase();
      if (SERVICE_KEYS.has(key)) return known.filter((s) => startsWith(s, t.text)).slice(0, LIMIT).map((s) => pair(t.key!, s));
      if (["level", "lvl", "severity", "sev"].includes(key)) return LEVELS.filter((l) => startsWith(l, t.text)).map((l) => pair(t.key!, l));
      return [];
    },
    remote: (t, signal) => {
      if (t.key === null) return attributes("logs", t, w, service, signal);
      const key = t.key.toLowerCase();
      if (SERVICE_KEYS.has(key) || NO_VALUES.has(key) || ["level", "lvl", "severity", "sev"].includes(key)) return null;
      return attributes("logs", t, w, service, signal);
    },
  };
}

export function issueSuggestions(project: string, w: string): SuggestSource {
  return {
    scope: ["issues", project, w],
    local: () => [],
    remote: (t, signal) => {
      const prefix = t.text || undefined;
      if (t.key) {
        const key = t.key;
        return sentry.tags(project, { since: w, key, prefix, limit: LIMIT }, signal).then((r) => ({
          countedFrom: cut(r.counted_from, w),
          items: r.items.map((i) => pair(key, i.value ?? "", i.count)),
        }));
      }
      return sentry.tags(project, { since: w, prefix, limit: LIMIT }, signal).then((r) => ({
        countedFrom: cut(r.counted_from, w),
        items: r.items.map((i) => ({ label: i.key, insert: `${i.key}=`, count: i.count })),
      }));
    },
  };
}
