import { describe, expect, it } from "vitest";
import { aggregations, metricTypeLabel, metricValue, seriesLabel } from "./metrics";

describe("metrics", () => {
  it("default aggregation per type", () => {
    expect(aggregations("gauge", "")[0]).toBe("avg");
    expect(aggregations("sum", "cumulative")[0]).toBe("rate");
    expect(aggregations("sum", "")[0]).toBe("avg");
    expect(aggregations("histogram", "delta")[0]).toBe("p95");
  });
  it("type labels", () => {
    expect(metricTypeLabel("sum")).toBe("counter");
    expect(metricTypeLabel("exponential_histogram")).toBe("exp hist");
  });
  it("values by unit", () => {
    expect(metricValue(1200, "count", "s", 0)).toBe("1.2k");
    expect(metricValue(2.5, "rate", "1", 0)).toBe("2.50/s");
    expect(metricValue(0.5, "avg", "1", 1)).toBe("50.0%");
    expect(metricValue(5, "avg", "1", 9)).toBe("5");
    expect(metricValue(0.25, "p95", "s", 1)).toBe("250.0ms");
    expect(metricValue(1500, "avg", "By", 1)).toBe("1.5 kB");
  });
  it("series labels", () => {
    expect(seriesLabel({ a: "x", b: "y" }, "avg")).toBe("x · y");
    expect(seriesLabel({}, "avg")).toBe("avg");
  });
});
