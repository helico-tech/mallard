import { describe, expect, it } from "vitest";
import { barGeometry, buildTree, epochMicros } from "./waterfall";

const span = (id: string, parent: string | null, startMs: number, durMs: number) => ({
  span_id: id,
  parent_span_id: parent,
  start_time: new Date(Date.UTC(2026, 0, 1) + startMs).toISOString(),
  duration_ns: durMs * 1e6,
});

describe("waterfall", () => {
  it("depth first, children by start time", () => {
    const t = buildTree([span("c", "a", 20, 5), span("a", null, 0, 100), span("b", "a", 10, 5), span("d", "b", 11, 1)]);
    expect(t.rows.map((r) => [r.span.span_id, r.depth])).toEqual([["a", 0], ["b", 1], ["d", 2], ["c", 1]]);
    expect(t.totalMs).toBe(100);
    expect(t.rows[0]!.descendants).toBe(3);
    expect(t.rows[3]!.offsetMs).toBe(20);
  });
  it("a missing parent becomes a marked root", () => {
    const t = buildTree([span("a", "gone", 0, 1), span("b", null, 1, 1)]);
    expect(t.rows.map((r) => [r.span.span_id, r.depth, r.orphan])).toEqual([["a", 0, true], ["b", 0, false]]);
  });
  it("cycles are kept, not lost or looping", () => {
    const t = buildTree([span("a", "b", 0, 1), span("b", "a", 1, 1), span("r", "r", 2, 1)]);
    expect(t.rows.map((r) => r.span.span_id).sort()).toEqual(["a", "b", "r"]);
  });
  it("very deep traces do not overflow", () => {
    const n = 20000;
    const spans = Array.from({ length: n }, (_, i) => span(`s${i}`, i ? `s${i - 1}` : null, i, 1));
    const tree = buildTree(spans);
    expect(tree.rows).toHaveLength(n);
    expect(tree.rows[0]!.descendants).toBe(n - 1);
    expect(tree.rows[n - 1]!.descendants).toBe(0);
  });
  it("collapsing hides the subtree and keeps the count", () => {
    const t = buildTree([span("a", null, 0, 9), span("b", "a", 1, 1), span("c", "b", 2, 1), span("d", null, 3, 1)], new Set(["a"]));
    expect(t.rows.map((r) => r.span.span_id)).toEqual(["a", "d"]);
    expect(t.rows[0]!.descendants).toBe(2);
    expect(t.totalMs).toBe(9);
  });
  it("microseconds survive", () => {
    expect(epochMicros("2026-01-01T00:00:00.000123Z") - epochMicros("2026-01-01T00:00:00Z")).toBeCloseTo(123, 3);
  });
  it("bars stay inside and visible", () => {
    expect(barGeometry(0, 50, 100)).toEqual({ left: 0, width: 50 });
    expect(barGeometry(100, 0, 100).left + barGeometry(100, 0, 100).width).toBeLessThanOrEqual(100);
    expect(barGeometry(5, 0, 100).width).toBe(0.4);
    expect(barGeometry(0, 0, 0)).toEqual({ left: 0, width: 100 });
  });
});
