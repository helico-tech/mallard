import type { Deploy } from "@/api/types";

export interface DeployMark {
  time: number;
  /** The version. */
  label: string;
  /** Where, and the deploy's name if it has one. */
  detail: string;
}

export function deployMarks(deploys: Deploy[]): DeployMark[] {
  return deploys.map((d) => ({
    time: Date.parse(d.date_finished),
    label: d.version,
    detail: d.name ? `${d.environment}: ${d.name}` : d.environment,
  }));
}

/** Where a mark falls in a span, from 0 to 1; null outside it. */
export function markFraction(time: number, start: number, span: number): number | null {
  if (span <= 0) return null;
  const f = (time - start) / span;
  return f >= 0 && f <= 1 ? f : null;
}
