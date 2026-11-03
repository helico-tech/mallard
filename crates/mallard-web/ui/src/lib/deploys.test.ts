import { describe, expect, it } from "vitest";
import { deployMarks, markFraction } from "./deploys";

describe("deploy marks", () => {
  it("names the version and where it went", () => {
    const d = { version: "web@2", environment: "production", date_finished: "2026-01-01T00:00:00Z" };
    expect(deployMarks([{ ...d, name: "first" }, { ...d, name: null }])).toEqual([
      { time: Date.parse(d.date_finished), label: "web@2", detail: "production: first" },
      { time: Date.parse(d.date_finished), label: "web@2", detail: "production" },
    ]);
  });
  it("places a mark in its span, and not outside it", () => {
    expect(markFraction(150, 100, 100)).toBe(0.5);
    expect(markFraction(100, 100, 100)).toBe(0);
    expect(markFraction(200, 100, 100)).toBe(1);
    expect(markFraction(99, 100, 100)).toBeNull();
    expect(markFraction(201, 100, 100)).toBeNull();
    expect(markFraction(100, 100, 0)).toBeNull();
  });
});
