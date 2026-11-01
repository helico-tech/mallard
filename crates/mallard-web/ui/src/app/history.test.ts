import { beforeEach, describe, expect, it } from "vitest";
import { backTarget, forgetApp, lastTelemetryView, record, reset, screenAddress } from "./history";

describe("back stacks, per side as in the TUI", () => {
  beforeEach(reset);

  it("back from an issue is the list with its filters, not the window", () => {
    record("/errors/web", "?status=all&w=7d");
    record("/errors/web/issues/1", "?w=7d");
    expect(backTarget("errors", "/x")).toBe("/errors/web?status=all");
  });

  it("the other side and its views do not change where back leads", () => {
    record("/errors/web", "");
    record("/errors/web/issues/1", "");
    record("/telemetry/overview", "");
    record("/telemetry/logs", "");
    record("/errors/web/issues/1", "");
    expect(backTarget("errors", "/x")).toBe("/errors/web");
  });

  it("an event opened from a trace goes back to the errors of that trace", () => {
    record("/errors/web", "");
    record("/errors-by-trace/t", "");
    record("/errors/web/issues/1", "?event=e");
    expect(backTarget("errors", "/x")).toBe("/errors-by-trace/t");
  });

  it("a trace jumped to goes back to the trace list of its own side", () => {
    record("/errors/web/issues/1", "");
    record("/telemetry/traces/abc", "");
    expect(backTarget("telemetry", "/telemetry/traces")).toBe("/telemetry/traces");
  });

  it("a view key starts its stack over", () => {
    record("/telemetry/traces", "?f=errors");
    record("/telemetry/traces/abc", "");
    record("/telemetry/logs", "");
    record("/telemetry/traces", "?f=errors");
    expect(backTarget("telemetry", "/fallback")).toBe("/fallback");
    expect(screenAddress("/telemetry/traces")).toBe("/telemetry/traces?f=errors");
  });

  it("going back truncates, and filter changes update the entry", () => {
    record("/telemetry/traces", "?f=a");
    record("/telemetry/traces", "?f=b");
    record("/telemetry/traces/abc", "");
    expect(backTarget("telemetry", "/x")).toBe("/telemetry/traces?f=b");
    record("/telemetry/traces", "?f=b");
    expect(backTarget("telemetry", "/fallback")).toBe("/fallback");
  });

  it("a tab returns to its detail screen and F2 to the last view", () => {
    record("/telemetry/metrics", "");
    record("/telemetry/metrics/a.b", "?agg=p95&w=1h");
    expect(screenAddress("/telemetry/metrics")).toBe("/telemetry/metrics/a.b?agg=p95");
    expect(lastTelemetryView()).toBe("/telemetry/metrics");
    expect(screenAddress("/errors")).toBe("/errors");
  });

  it("the app and the window are never restored; another app forgets its screens", () => {
    record("/telemetry/traces", "?f=errors&w=6h&app=web");
    expect(screenAddress("/telemetry/traces")).toBe("/telemetry/traces?f=errors");
    record("/errors/web", "?status=all");
    record("/telemetry/logs/x/y", "");
    forgetApp();
    expect(screenAddress("/errors")).toBe("/errors");
    expect(screenAddress("/telemetry/traces")).toBe("/telemetry/traces?f=errors");
    expect(screenAddress("/telemetry/logs")).toBe("/telemetry/logs");
  });
});
