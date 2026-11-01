// One format module, same outputs as `mallard_term::fmt`. All times are UTC.

function stripZero(s: string): string {
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

/** `999`, `1.2k`, `3.4M`, `2G`, `2.50`, `0.250`. */
export function compact(v: number): string {
  if (!Number.isFinite(v)) return "–";
  const abs = Math.abs(v);
  if (abs < 1000) {
    if (Number.isInteger(v) || abs >= 100) return v.toFixed(0);
    return abs >= 1 ? v.toFixed(2) : v.toFixed(3);
  }
  const units: [number, string][] = [[1e12, "T"], [1e9, "G"], [1e6, "M"], [1e3, "k"]];
  for (let i = 0; i < units.length; i++) {
    const [size, unit] = units[i]!;
    if (abs >= size) {
      const text = (v / size).toFixed(1);
      // 999,999 rounds to "1000.0k": that is 1M.
      if (Math.abs(Number(text)) >= 1000 && i > 0) {
        return `${stripZero((v / units[i - 1]![0]).toFixed(1))}${units[i - 1]![1]}`;
      }
      return `${stripZero(text)}${unit}`;
    }
  }
  return String(v);
}

/** Decimal, as the config files write them: `512 B`, `1.5 MB`. */
export function bytes(v: number): string {
  if (!Number.isFinite(v)) return "–";
  if (Math.abs(v) < 1000) return `${v.toFixed(0)} B`;
  const units = ["kB", "MB", "GB", "TB", "PB"];
  let n = v;
  let unit = "B";
  for (const u of units) {
    if (Math.abs(n) < 1000) break;
    n /= 1000;
    unit = u;
  }
  return `${n.toFixed(1)} ${unit}`;
}

/** `500µs`, `12.3ms`, `2.50s`, `1.5m`. */
export function ms(v: number): string {
  if (!Number.isFinite(v)) return "–";
  if (v < 1) return `${(v * 1000).toFixed(0)}µs`;
  if (v < 1000) return `${v.toFixed(1)}ms`;
  if (v < 60_000) return `${(v / 1000).toFixed(2)}s`;
  return `${(v / 60_000).toFixed(1)}m`;
}

const toDate = (t: string | Date): Date => (t instanceof Date ? t : new Date(t));
const pad = (n: number, w = 2): string => String(n).padStart(w, "0");

/** `2026-10-31 20:59:13`, UTC. */
export function timestamp(t: string | Date): string {
  const d = toDate(t);
  if (Number.isNaN(d.getTime())) return "–";
  return d.toISOString().slice(0, 19).replace("T", " ");
}

/** `20:59:13.123`, UTC. */
export function timeMs(t: string | Date): string {
  const d = toDate(t);
  if (Number.isNaN(d.getTime())) return "–";
  return d.toISOString().slice(11, 23);
}

/** `10-31 20:59:13`, UTC. */
export function dayTime(t: string | Date): string {
  const d = toDate(t);
  if (Number.isNaN(d.getTime())) return "–";
  return d.toISOString().slice(5, 19).replace("T", " ");
}

/** `2026-10-31 20:59:13.123456`, UTC; keeps the digits the server sent. */
export function timestampFull(t: string): string {
  const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})(?:\.(\d+))?/.exec(t);
  if (!m) return timestamp(t);
  return `${m[1]} ${m[2]}.${(m[3] ?? "").padEnd(6, "0").slice(0, 6)}`;
}

/** Chart axis label: the shorter the span, the finer. */
export function axisTime(t: number, spanMs: number): string {
  const iso = new Date(t).toISOString();
  if (spanMs > 2 * 86_400_000) return iso.slice(5, 16).replace("T", " ");
  if (spanMs > 3 * 3_600_000) return iso.slice(11, 16);
  return iso.slice(11, 19);
}

function ageParts(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h`;
  if (seconds < 30 * 86_400) return `${Math.floor(seconds / 86_400)}d`;
  if (seconds < 365 * 86_400) return `${Math.floor(seconds / (30 * 86_400))}mo`;
  return `${Math.floor(seconds / (365 * 86_400))}y`;
}

/** `now`, `42s ago`, `3m ago`, `2d ago`, `2mo ago`. */
export function relative(t: string | Date, now: number = Date.now()): string {
  const s = Math.floor((now - toDate(t).getTime()) / 1000);
  if (Number.isNaN(s)) return "–";
  return s < 5 ? "now" : `${ageParts(s)} ago`;
}

/** `3m`, `now`: `relative` without the `ago`. */
export function age(t: string | Date, now: number = Date.now()): string {
  return relative(t, now).replace(/ ago$/, "");
}

/** `45s`, `2m 5s`, `1h 2m`, `3d 4h`. */
export function uptime(secs: number): string {
  const s = Math.floor(secs);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86_400)}d ${Math.floor((s % 86_400) / 3600)}h`;
}

/** Error rate as `x.xx%`. */
export function percent(rate: number): string {
  return `${(rate * 100).toFixed(2)}%`;
}

export function short(id: string | null | undefined, n = 8): string {
  return id ? id.slice(0, n) : "–";
}

export { pad };
