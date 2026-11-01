// Follow logs by polling (`odl-client::LogTail`): the server has no streaming.
// Each poll looks back 30s for late records and dedupes by identity, counted,
// so two identical records stay two.

export interface TailRecord {
  time: string;
  body: string;
  service_name: string;
  severity_number: number;
  trace_id?: string | null;
  span_id?: string | null;
}

export const LOOKBACK_MS = 30_000;
export const MAX_GAP_MS = 10 * 60_000;
export const POLL_LIMIT = 500;

const key = (r: TailRecord) => `${r.time}|${r.service_name}|${r.severity_number}|${r.trace_id ?? ""}|${r.span_id ?? ""}|${r.body}`;

/** The window of a poll `elapsed` ms after the last, and whether it was capped. */
export function pollWindow(elapsedMs: number): { backMs: number; capped: boolean } {
  const want = elapsedMs + LOOKBACK_MS + 1000;
  const cap = Math.max(MAX_GAP_MS, LOOKBACK_MS + 1000);
  return want > cap ? { backMs: cap, capped: true } : { backMs: want, capped: false };
}

export class Tail<R extends TailRecord> {
  private seen = new Map<string, number>();
  private lastPoll: number | null = null;

  constructor(private readonly start: Date) {}

  markSeen(records: R[]): void {
    const counts = new Map<string, number>();
    for (const r of records) counts.set(key(r), (counts.get(key(r)) ?? 0) + 1);
    for (const [k, n] of counts) this.seen.set(k, Math.max(this.seen.get(k) ?? 0, n));
  }

  /** Query window of the next poll. */
  window(now: number): { from?: string; since?: string; capped: boolean } {
    if (this.lastPoll === null) {
      return { from: new Date(this.start.getTime() - LOOKBACK_MS).toISOString(), capped: false };
    }
    const { backMs, capped } = pollWindow(now - this.lastPoll);
    return { since: `${backMs}ms`, capped };
  }

  /** One response, newest first. New records oldest first, and whether some may be missing. */
  accept(records: R[], limit: number, startedAt: number, capped: boolean): { fresh: R[]; truncated: boolean } {
    const budget = new Map(this.seen);
    const fresh: R[] = [];
    let reachedSeen = false;
    // Oldest first, so a partly seen burst keeps its order.
    for (let i = records.length - 1; i >= 0; i--) {
      const r = records[i]!;
      const k = key(r);
      const left = budget.get(k) ?? 0;
      if (left > 0) {
        budget.set(k, left - 1);
        reachedSeen = true;
      } else {
        fresh.push(r);
        this.seen.set(k, (this.seen.get(k) ?? 0) + 1);
      }
    }
    if (this.seen.size > 200_000) this.seen = new Map([...this.seen].slice(-100_000));
    this.lastPoll = startedAt;
    const full = records.length >= limit;
    return { fresh, truncated: capped || (full && !reachedSeen && this.seen.size > fresh.length) };
  }
}
