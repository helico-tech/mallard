import { describe, expect, it } from "vitest";
import { breadcrumbs, context, contexts, exceptions, framesNewestFirst, functionName, location, mechanismLabel, message, request } from "./event";

const payload = {
  exception: { values: [
    { type: "TypeError", value: "root cause", mechanism: { type: "chained", handled: true },
      stacktrace: { frames: [{ function: "main", filename: "node:internal/main", lineno: 1, in_app: false }] } },
    { type: "Error", value: "inside span", mechanism: { type: "generic", handled: false },
      stacktrace: { frames: [
        { function: "runMain", filename: "node:internal/run_main", lineno: 98, colno: 5, in_app: false },
        { function: "parseConfig", filename: "/app/smoke.mjs", lineno: 11, colno: 84, in_app: true,
          pre_context: ["line nine", "line ten"], context_line: "throw", post_context: ["line twelve"] },
      ] },
      raw_stacktrace: { frames: [{ function: "n", filename: "/app/smoke.min.js", lineno: 1, in_app: true }] } },
  ] },
  breadcrumbs: { values: [
    { category: "auth", message: "user logged in", timestamp: 1793480353.983 },
    { type: "http", timestamp: "2026-10-31T20:59:14Z", data: { method: "GET", url: "/api/cart", status_code: 500 } },
  ] },
  request: { method: "POST", url: "https://shop.example/checkout" },
  contexts: { runtime: { name: "node", version: "v24" }, os: { name: "macOS" }, device: { model: "Mac" }, trace: {} },
};

describe("event view matches event_view.rs", () => {
  it("exception chain: primary first, frames newest first", () => {
    const ex = exceptions(payload);
    expect(ex.map((e) => e.type)).toEqual(["Error", "TypeError"]);
    expect(functionName(framesNewestFirst(ex[0]!, false)[0]!)).toBe("parseConfig");
    expect(functionName(framesNewestFirst(ex[0]!, true)[0]!)).toBe("n");
    expect(mechanismLabel(ex[0]!)).toBe("generic · unhandled");
  });
  it("location and source context", () => {
    const f = framesNewestFirst(exceptions(payload)[0]!, false)[0]!;
    expect(location(f)).toBe("/app/smoke.mjs:11:84");
    expect(context(f).map((c) => [c.line, c.error])).toEqual([[9, false], [10, false], [11, true], [12, false]]);
    expect(functionName({ function: "?", inApp: false, preContext: [], postContext: [] })).toBe("<anonymous>");
  });
  it("pseudo exception from a crashed thread, then top level", () => {
    const fromThread = exceptions({ threads: { values: [
      { stacktrace: { frames: [{ function: "other" }] } },
      { crashed: true, stacktrace: { frames: [{ function: "crash" }] } },
    ] }, message: "boom" });
    expect(fromThread[0]!.type).toBe("Stack trace");
    expect(fromThread[0]!.value).toBe("boom");
    expect(functionName(fromThread[0]!.frames[0]!)).toBe("crash");
    expect(exceptions({ stacktrace: { frames: [{ function: "t" }] } })).toHaveLength(1);
    expect(exceptions({})).toEqual([]);
  });
  it("breadcrumbs newest first with a data fallback", () => {
    const b = breadcrumbs(payload);
    expect(b[0]!.text).toBe("GET /api/cart -> 500");
    expect(b[0]!.category).toBe("http");
    expect(b[1]!.level).toBe("info");
    expect(breadcrumbs({ breadcrumbs: [{ data: { a: 1, b: "x" } }] })[0]!.text).toBe("a=1 b=x");
  });
  it("contexts in fixed order, request, message", () => {
    expect(contexts(payload)).toEqual([["runtime", "node v24"], ["os", "macOS"], ["device", "Mac"]]);
    expect(request(payload)).toBe("POST https://shop.example/checkout");
    expect(message({ logentry: { formatted: "f", message: "m" } })).toBe("f");
    expect(message({ message: { message: "x" } })).toBe("x");
    expect(message({ message: "" })).toBeUndefined();
  });
  it("is lenient about nonsense", () => {
    expect(exceptions(null)).toEqual([]);
    expect(exceptions({ exception: "no" })).toEqual([]);
    expect(breadcrumbs(3)).toEqual([]);
  });
});
