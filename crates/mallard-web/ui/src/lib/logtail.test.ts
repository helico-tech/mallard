import { describe, expect, it } from "vitest";
import { LOOKBACK_MS, MAX_GAP_MS, Tail, pollWindow } from "./logtail";

const rec = (t: number, body = "b") => ({ time: new Date(t).toISOString(), body, service_name: "s", severity_number: 9 });

describe("log tail", () => {
  it("first poll starts 30s before the start, later ones are relative", () => {
    const start = new Date(100_000);
    const tail = new Tail(start);
    expect(tail.window(100_000)).toEqual({ from: new Date(100_000 - LOOKBACK_MS).toISOString(), capped: false });
    tail.accept([], 500, 100_000, false);
    expect(tail.window(102_000)).toEqual({ since: "33000ms", capped: false });
  });
  it("caps long gaps", () => {
    expect(pollWindow(60 * 60_000)).toEqual({ backMs: MAX_GAP_MS, capped: true });
    expect(pollWindow(1000).capped).toBe(false);
  });
  it("returns only new records, oldest first", () => {
    const tail = new Tail(new Date(0));
    expect(tail.accept([rec(2), rec(1)], 500, 0, false).fresh.map((r) => r.time)).toEqual([rec(1).time, rec(2).time]);
    expect(tail.accept([rec(3), rec(2), rec(1)], 500, 1, false).fresh).toEqual([rec(3)]);
  });
  it("identical records stay separate, and are not repeated", () => {
    const tail = new Tail(new Date(0));
    expect(tail.accept([rec(1), rec(1)], 500, 0, false).fresh).toHaveLength(2);
    expect(tail.accept([rec(1), rec(1)], 500, 1, false).fresh).toHaveLength(0);
    expect(tail.accept([rec(1), rec(1), rec(1)], 500, 2, false).fresh).toHaveLength(1);
  });
  it("marks a gap when a full page does not reach seen records", () => {
    const tail = new Tail(new Date(0));
    tail.markSeen([rec(1)]);
    expect(tail.accept([rec(4), rec(3)], 2, 0, false).truncated).toBe(true);
    expect(tail.accept([rec(4), rec(3)], 2, 1, false).truncated).toBe(false);
    expect(tail.accept([rec(9)], 500, 2, true).truncated).toBe(true);
  });
  it("markSeen hides an initial page", () => {
    const tail = new Tail(new Date(0));
    tail.markSeen([rec(1)]);
    expect(tail.accept([rec(1)], 500, 0, false).fresh).toEqual([]);
  });
});
