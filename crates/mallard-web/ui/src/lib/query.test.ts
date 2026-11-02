import { describe, expect, it } from "vitest";
import { cycleLevel, parseDurationMs, parseFindId, parseIssueFilter, parseLogFilter, parseSeverity, parseTraceFilter, parseTraceId, setToken, tokens } from "./query";

describe("filter language", () => {
  it("tokens keep quotes together", () => {
    expect(tokens('a "b c" d=e  f="g h"')).toEqual(["a", "b c", "d=e", "f=g h"]);
    expect(tokens("")).toEqual([]);
    expect(tokens('"" a')).toEqual(["", "a"]);
  });
  it("durations", () => {
    expect(parseDurationMs("250")).toBe(250);
    expect(parseDurationMs("250ms")).toBe(250);
    expect(parseDurationMs("1.5s")).toBe(1500);
    expect(parseDurationMs("2m")).toBe(120000);
    expect(parseDurationMs("100us")).toBe(0.1);
    expect(parseDurationMs(".5s")).toBe(500);
    expect(parseDurationMs("5.")).toBe(5);
    expect(parseDurationMs(".")).toBeNull();
    expect(parseDurationMs("soon")).toBeNull();
  });
  it("trace filters", () => {
    expect(parseTraceFilter('svc=api op="GET /x" min=1s max=2s errors >200 http.route=/y')).toEqual({
      service: "api", operation: "GET /x", minMs: 200, maxMs: 2000, errorsOnly: true, attributes: ["http.route=/y"],
    });
    expect(parseTraceFilter("errors=no")).toMatchObject({ errorsOnly: false });
    expect(parseTraceFilter("banana")).toHaveProperty("error");
    expect(parseTraceFilter("min=later")).toHaveProperty("error");
  });
  it("log filters: bare words are the text search", () => {
    expect(parseLogFilter('s=api level=warn trace=abc order placed k=v')).toEqual({
      service: "api", minSeverity: 13, trace: "abc", q: "order placed", attributes: ["k=v"],
    });
    expect(parseLogFilter("level=loud")).toHaveProperty("error");
  });
  it("severity", () => {
    expect(parseSeverity("warning")).toBe(13);
    expect(parseSeverity("24")).toBe(24);
    expect(parseSeverity("25")).toBeNull();
  });
  it("s cycles the level token and keeps the rest", () => {
    expect(cycleLevel("")).toBe("level=info");
    expect(cycleLevel("level=info s=api")).toBe("level=warn s=api");
    expect(cycleLevel("s=api level=warn boom")).toBe("level=error s=api boom");
    expect(cycleLevel("level=error s=api")).toBe("s=api");
    expect(cycleLevel("level=debug")).toBe("level=warn");
    expect(cycleLevel("level=fatal")).toBe("");
  });
  it("setToken replaces", () => {
    expect(setToken("trace=a x", "trace", "b")).toBe("x trace=b");
    expect(setToken("", "op", "GET /x")).toBe('op="GET /x"');
  });
  it("trace ids", () => {
    expect(parseTraceId("0AF76519-16cd-43dd-8448-eb211c80319c")).toBe("0af7651916cd43dd8448eb211c80319c");
    expect(parseTraceId("abc")).toBeNull();
  });

  it("issue filter: key=value is a tag, other words search the text", () => {
    expect(parseIssueFilter('undefined total browser="Chrome 120"')).toEqual({ tags: ["browser:Chrome 120"], query: "undefined total" });
    expect(parseIssueFilter("")).toEqual({ tags: [] });
    expect(parseIssueFilter("url=/a=b")).toEqual({ tags: ["url:/a=b"] });
    expect(parseIssueFilter(Array.from({ length: 11 }, (_, i) => `k${i}=v`).join(" "))).toHaveProperty("error");
  });

  it("issue filter: an = in quotes is text", () => {
    expect(parseIssueFilter('"a=b"')).toEqual({ tags: [], query: "a=b" });
    expect(parseIssueFilter('"a=b c" x')).toEqual({ tags: [], query: "a=b c x" });
    expect(parseIssueFilter('"a"=b')).toEqual({ tags: [], query: "a=b" });
    expect(parseIssueFilter("a:b=c")).toEqual({ tags: [], query: "a:b=c" });
    // As in the TUI: any whitespace, and keys are case-sensitive.
    expect(parseIssueFilter("a=b\tc=d")).toEqual({ tags: ["a:b", "c:d"] });
    expect(parseTraceFilter("Service=x")).toEqual({ errorsOnly: false, attributes: ["Service=x"] });
    expect(parseLogFilter("Level=error")).toEqual({ attributes: ["Level=error"] });
    expect(parseIssueFilter('"a=b" c=d')).toEqual({ tags: ["c:d"], query: "a=b" });
    expect(parseIssueFilter('a="b=c"')).toEqual({ tags: ["a:b=c"] });
  });

  it("find ids: 16 or 32 hex, in any form", () => {
    expect(parseFindId(" 0AF7651916CD43DD8448EB211C80319C ")).toBe("0af7651916cd43dd8448eb211c80319c");
    expect(parseFindId("0af76519-16cd-43dd-8448-eb211c80319c")).toBe("0af7651916cd43dd8448eb211c80319c");
    expect(parseFindId("b7ad6b7169203331")).toBe("b7ad6b7169203331");
    expect(parseFindId("checkout-api")).toBeNull();
    expect(parseFindId("abc")).toBeNull();
  });
});
