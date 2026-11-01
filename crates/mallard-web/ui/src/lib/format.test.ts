import { describe, expect, it } from "vitest";
import { age, bytes, compact, dayTime, ms, percent, relative, timeMs, timestamp, timestampFull, uptime } from "./format";

describe("format matches mallard_term::fmt", () => {
  it("compact", () => {
    const cases: [number, string][] = [
      [0, "0"], [999, "999"], [1000, "1k"], [1200, "1.2k"], [12300, "12.3k"], [999_999, "1M"],
      [3_400_000, "3.4M"], [2e9, "2G"], [2.5, "2.50"], [0.25, "0.250"], [-1500, "-1.5k"], [150.7, "151"],
    ];
    for (const [v, want] of cases) expect(compact(v)).toBe(want);
  });
  it("bytes", () => {
    expect(bytes(512)).toBe("512 B");
    expect(bytes(1500)).toBe("1.5 kB");
    expect(bytes(1_500_000)).toBe("1.5 MB");
    expect(bytes(2_000_000_000)).toBe("2.0 GB");
    expect(bytes(512_000_000)).toBe("512.0 MB");
  });
  it("ms", () => {
    expect(ms(0.5)).toBe("500µs");
    expect(ms(12.3)).toBe("12.3ms");
    expect(ms(2500)).toBe("2.50s");
    expect(ms(90_000)).toBe("1.5m");
  });
  it("relative and age", () => {
    const now = Date.parse("2026-11-01T12:00:00Z");
    const ago = (s: number) => new Date(now - s * 1000);
    expect(relative(ago(2), now)).toBe("now");
    expect(relative(ago(-30), now)).toBe("now");
    expect(relative(ago(42), now)).toBe("42s ago");
    expect(relative(ago(180), now)).toBe("3m ago");
    expect(relative(ago(5 * 3600), now)).toBe("5h ago");
    expect(relative(ago(2 * 86400), now)).toBe("2d ago");
    expect(relative(ago(60 * 86400), now)).toBe("2mo ago");
    expect(relative(ago(730 * 86400), now)).toBe("2y ago");
    expect(age(ago(180), now)).toBe("3m");
    expect(age(ago(3), now)).toBe("now");
  });
  it("times are UTC", () => {
    expect(timestamp("2026-10-31T22:59:13+02:00")).toBe("2026-10-31 20:59:13");
    expect(timeMs("2026-10-31T20:59:13.123Z")).toBe("20:59:13.123");
    expect(dayTime("2026-10-31T20:59:13Z")).toBe("10-31 20:59:13");
    expect(timestampFull("2026-10-31T20:59:13.12345678Z")).toBe("2026-10-31 20:59:13.123456");
    expect(timestamp("nonsense")).toBe("–");
  });
  it("uptime and percent", () => {
    expect(uptime(45)).toBe("45s");
    expect(uptime(125)).toBe("2m 5s");
    expect(uptime(3720)).toBe("1h 2m");
    expect(uptime(273600)).toBe("3d 4h");
    expect(percent(0.0123)).toBe("1.23%");
  });
});
