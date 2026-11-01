// The metrics screen's rules (`odl-cli/src/tui/views/metrics.rs`).
import { bytes, compact, ms } from "./format";

export function metricTypeLabel(t: string): string {
  switch (t.toLowerCase()) {
    case "sum":
      return "counter";
    case "exponential_histogram":
    case "exponentialhistogram":
      return "exp hist";
    default:
      return t.toLowerCase();
  }
}

/** First is the default. */
export function aggregations(type: string, temporality: string): string[] {
  switch (type.toLowerCase()) {
    case "gauge":
      return ["avg", "min", "max", "sum", "count"];
    case "sum":
      return temporality && temporality.toLowerCase() !== "unspecified"
        ? ["rate", "increase", "avg", "max", "sum"]
        : ["avg", "min", "max", "sum"];
    default:
      return ["p95", "p50", "p90", "p99", "avg", "count", "rate"];
  }
}

export function metricValue(v: number, agg: string, unit: string, max: number): string {
  if (agg === "count" || agg === "increase") return compact(v);
  if (agg === "rate") return `${compact(v)}/s`;
  switch (unit) {
    case "1":
      return max <= 1 ? `${(v * 100).toFixed(1)}%` : compact(v);
    case "s":
      return ms(v * 1000);
    case "ms":
      return ms(v);
    case "By":
      return bytes(v);
    default:
      return compact(v);
  }
}

/** Label values joined, or the aggregation name when there are no labels. */
export function seriesLabel(labels: Record<string, string>, agg: string): string {
  const vals = Object.values(labels);
  return vals.length ? vals.join(" · ") : agg;
}
