// @sentry/node with tracing on, through mallard's bridge (ADR 0009): the
// SDK's transactions become OTel spans in /otel, and the error of the
// request sits in that trace on the span it was thrown in, a child of the
// request's span.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { TestServer, exceptions, waitFor } from "../lib/server.mjs";
import { runFixture } from "../lib/node.mjs";

let server;
let project;

before(async () => {
  server = await TestServer.start("tracing");
  project = await server.createProject("Tracing App");
});

after(async () => {
  await server?.stop();
});

/** The trace once it has the request span and both child spans (spans are buffered: poll). */
function waitForTrace(run, timeout = 30_000) {
  return waitFor(
    async () => {
      const t = await server.otel("GET", `/api/v1/traces/${run.trace_id}`).catch((e) => {
        if (e.status === 404) return null;
        throw e;
      });
      const ids = new Set(t?.spans.map((s) => s.span_id));
      return ids.has(run.server_span_id) && ids.has(run.charge_span_id) && t.spans.some((s) => s.name === "load cart") && t;
    },
    { timeout, what: `trace ${run.trace_id} in /otel/api/v1/traces` },
  );
}

async function checkout(lifecycle, tag) {
  const r = await runFixture("node-tracing-app.mjs", project.dsn, [lifecycle, tag]);
  assert.equal(r.code, 0, `node-tracing-app failed:\n${r.stderr}`);
  const run = r.last;
  assert.equal(run?.status, 500, r.stdout);
  assert.match(run.trace_id, /^[0-9a-f]{32}$/);
  assert.match(run.server_span_id, /^[0-9a-f]{16}$/);
  assert.match(run.charge_span_id, /^[0-9a-f]{16}$/);
  return run;
}

test("transactions (traceLifecycle static): spans in /otel, the error under the request span", async () => {
  const run = await checkout("static", "t1");

  // The error event.
  const ev = await server.event(project.slug, run.event_id);
  assert.equal(ev.title, "Error: card declined t1");
  const thrown = exceptions(ev).at(-1).stacktrace.frames.at(-1);
  assert.equal(thrown.function, "declineCard");
  assert.match(thrown.filename, /fixtures\/node-tracing-app\.mjs$/);
  // The SDK ties the error to the span it was thrown in.
  assert.equal(ev.trace_id, run.trace_id);
  assert.equal(ev.span_id, run.charge_span_id);
  assert.equal(ev.payload.contexts.trace.parent_span_id, run.server_span_id);

  // The trace, through the bridge.
  const trace = await waitForTrace(run);
  assert.equal(trace.trace_id, run.trace_id);
  const byId = new Map(trace.spans.map((s) => [s.span_id, s]));
  const root = byId.get(run.server_span_id);
  assert.equal(root.kind, "SERVER");
  assert.equal(root.attributes["sentry.op"], "http.server");
  assert.equal(root.service_name, project.slug, "service.name falls back to the project slug");
  assert.equal(root.resource_attributes["service.version"], "tracing-app@1.0.0");
  assert.equal(root.resource_attributes["deployment.environment.name"], "e2e");
  // Both child spans hang under the request span; the throwing one is an error.
  const load = trace.spans.find((s) => s.name === "load cart");
  assert.equal(load.parent_span_id, run.server_span_id);
  assert.equal(load.attributes["sentry.op"], "db.query");
  const charge = byId.get(run.charge_span_id);
  assert.equal(charge.name, "charge card");
  assert.equal(charge.parent_span_id, run.server_span_id);
  assert.equal(charge.status_code, "ERROR");
  assert.equal(charge.attributes["sentry.op"], "payment.charge");
  // The caller's transaction is in the same trace; the request span hangs under its fetch span.
  const client = trace.spans.find((s) => s.name === "checkout client");
  assert.ok(client, `caller span in ${JSON.stringify(trace.spans.map((s) => s.name))}`);
  const fetchSpan = byId.get(root.parent_span_id);
  assert.equal(fetchSpan?.kind, "CLIENT", "the request span's parent is the caller's fetch span");

  // Correlation the other way: the trace's errors, found by trace id, point at that span.
  const byTrace = await server.api("GET", `/v1/events?trace_id=${run.trace_id}`);
  assert.deepEqual(
    byTrace.events.map((e) => [e.event_id, e.span_id]),
    [[run.event_id, run.charge_span_id]],
  );
});

// @sentry/node 11 streams spans by default (`span` envelope items, v2); the
// bridge only converts `transaction` items, so these spans are dropped.
test("span streaming (SDK 11 default): spans in /otel", { todo: "work/issues/0017: bridge Sentry span v2 items" }, async () => {
  const run = await checkout("stream", "s1");
  const ev = await server.event(project.slug, run.event_id);
  assert.equal(ev.trace_id, run.trace_id, "the error is stored either way");
  await waitForTrace(run, 10_000);
});
