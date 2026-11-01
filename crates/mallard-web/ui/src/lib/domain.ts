// Domain meanings the TUI gives colours to: levels, severities, statuses,
// error rates, services. Colours are tokens from index.css; text always comes
// with them, so nothing is told by colour alone.

export type Tone = "fatal" | "error" | "warn" | "info" | "debug" | "muted";

/** Issue level (`mallard-sentry-cli/src/tui/theme.rs`). */
export function levelTone(level: string): Tone {
  switch (level.toLowerCase()) {
    case "fatal":
      return "fatal";
    case "error":
      return "error";
    case "warning":
    case "warn":
      return "warn";
    case "info":
      return "info";
    case "debug":
      return "debug";
    default:
      return "muted";
  }
}

export function levelLabel(level: string): string {
  switch (level.toLowerCase()) {
    case "fatal":
      return "Fatal";
    case "error":
      return "Error";
    case "warning":
    case "warn":
      return "Warn";
    case "info":
      return "Info";
    case "debug":
      return "Debug";
    default:
      return "Other";
  }
}

/** 1–4 TRACE, 5–8 DEBUG, 9–12 INFO, 13–16 WARN, 17–20 ERROR, 21–24 FATAL; else from the text. */
export function severityName(number: number, text: string): string {
  if (number >= 21 && number <= 24) return "FATAL";
  if (number >= 17) return number <= 20 ? "ERROR" : "-";
  if (number >= 13) return "WARN";
  if (number >= 9) return "INFO";
  if (number >= 5) return "DEBUG";
  if (number >= 1) return "TRACE";
  const t = text.toUpperCase();
  for (const name of ["FATAL", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"]) if (t.startsWith(name)) return name;
  return "-";
}

export function severityTone(name: string): Tone {
  switch (name) {
    case "FATAL":
      return "fatal";
    case "ERROR":
      return "error";
    case "WARN":
      return "warn";
    case "INFO":
      return "info";
    case "DEBUG":
    case "TRACE":
      return "debug";
    default:
      return "muted";
  }
}

/** ≥ 5% red, ≥ 1% yellow, > 0 subtle warning, 0 good. */
export function rateClass(rate: number): string {
  if (rate >= 0.05) return "text-kumo-danger font-medium";
  if (rate >= 0.01) return "text-kumo-warning";
  if (rate > 0) return "text-kumo-default";
  return "text-kumo-subtle";
}

/** FNV-1a 32-bit over UTF-8, as the TUI: a service keeps its colour everywhere. */
export function fnv1a(s: string): number {
  let h = 0x811c9dc5;
  for (const b of new TextEncoder().encode(s)) {
    h ^= b;
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

/** Kumo's categorical chart palette has six hues. */
export const SERIES_COUNT = 6;

/** CSS variable of a categorical slot (set from ChartPalette, see settings.tsx). */
export const seriesVar = (i: number): string => `var(--series-${(i % SERIES_COUNT) + 1})`;

export function serviceColor(name: string): string {
  return seriesVar(fnv1a(name) % SERIES_COUNT);
}

export const STATUSES = ["unresolved", "resolved", "ignored", "all"] as const;
export type StatusFilter = (typeof STATUSES)[number];

export const SORTS = [
  { value: "last_seen", label: "Last seen" },
  { value: "first_seen", label: "First seen" },
  { value: "count", label: "Events" },
  { value: "users", label: "Users" },
] as const;

/** One list for both sides (ADR 0028), with the bars the errors side draws. */
export const WINDOWS = [
  { w: "5m", interval: "10s" },
  { w: "15m", interval: "30s" },
  { w: "1h", interval: "1m" },
  { w: "6h", interval: "10m" },
  { w: "24h", interval: "30m" },
  { w: "7d", interval: "3h" },
  { w: "14d", interval: "6h" },
  { w: "30d", interval: "12h" },
] as const;
export const DEFAULT_WINDOW = "1h";

export const intervalOf = (w: string): string => WINDOWS.find((x) => x.w === w)?.interval ?? "30m";

/** The presets every side accepts. Each server says its own limit; one not known yet
 *  (or a side that is down) does not limit anything. Always leaves the smallest. */
export function windowsWithin(limits: (number | null | undefined)[]): string[] {
  const known = limits.filter((l): l is number => typeof l === "number" && l > 0);
  const max = known.length ? Math.min(...known) : Infinity;
  const fits = WINDOWS.map((x) => x.w as string).filter((w) => windowSeconds(w) <= max);
  return fits.length ? fits : [WINDOWS[0].w];
}

export function windowSeconds(w: string): number {
  const m = /^(\d+)([smhd])$/.exec(w);
  if (!m) return 3600;
  return Number(m[1]) * { s: 1, m: 60, h: 3600, d: 86400 }[m[2] as "s" | "m" | "h" | "d"];
}

/** Sums when shrinking so totals stay true; repeats when growing. */
export function resample(values: number[], width: number): number[] {
  if (values.length === 0 || width <= 0) return [];
  if (values.length === width) return values;
  if (values.length > width) {
    const out = new Array<number>(width).fill(0);
    values.forEach((v, i) => {
      out[Math.floor((i * width) / values.length)]! += v;
    });
    return out;
  }
  return Array.from({ length: width }, (_, i) => values[Math.floor((i * values.length) / width)]!);
}
