import { describe, expect, it } from "vitest";
import { fnv1a, levelLabel, resample, severityName, windowSeconds, windowsWithin } from "./domain";

describe("domain", () => {
  it("presets are cut to the smallest limit a side reports", () => {
    expect(windowsWithin([])).toEqual(["5m", "15m", "1h", "6h", "24h", "7d", "14d", "30d"]);
    expect(windowsWithin([undefined, 366 * 86400])).toContain("30d");
    expect(windowsWithin([366 * 86400, 7 * 86400]).at(-1)).toBe("7d");
    expect(windowsWithin([60])).toEqual(["5m"]);
  });
  it("severity names", () => {
    expect(severityName(9, "")).toBe("INFO");
    expect(severityName(24, "")).toBe("FATAL");
    expect(severityName(0, "warning")).toBe("WARN");
    expect(severityName(0, "?")).toBe("-");
    expect(severityName(0, "err")).toBe("ERROR");
    expect(severityName(25, "warn")).toBe("WARN");
    expect(severityName(25, "")).toBe("-");
    expect([1, 4, 5, 8, 9, 12, 13, 16, 17, 20, 21].map((n) => severityName(n, ""))).toEqual([
      "TRACE", "TRACE", "DEBUG", "DEBUG", "INFO", "INFO", "WARN", "WARN", "ERROR", "ERROR", "FATAL",
    ]);
  });
  it("level labels", () => {
    expect(levelLabel("warning")).toBe("Warn");
    expect(levelLabel("weird")).toBe("Other");
  });
  it("fnv1a known values", () => {
    expect(fnv1a("")).toBe(0x811c9dc5);
    expect(fnv1a("a")).toBe(0xe40c292c);
  });
  it("resample keeps totals when shrinking", () => {
    const v = [1, 2, 3, 4, 5, 6];
    expect(resample(v, 3).reduce((a, b) => a + b)).toBe(21);
    expect(resample(v, 6)).toEqual(v);
    expect(resample([1, 2], 4)).toEqual([1, 1, 2, 2]);
    expect(resample([], 4)).toEqual([]);
  });
  it("windows", () => {
    expect(windowSeconds("15m")).toBe(900);
    expect(windowSeconds("7d")).toBe(604800);
  });
});
