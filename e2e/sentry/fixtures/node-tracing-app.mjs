// A small Node HTTP service with @sentry/node tracing on. Only the DSN
// points at mallard. Usage: DSN=... node node-tracing-app.mjs <static|stream> [tag]
//
// `stream` is SDK 11's default span streaming (`span` items), `static` sends
// each local root span as a `transaction` item. mallard's bridge turns both
// into OTel spans.
//
// It serves `GET /checkout`, which runs two child spans; the second one
// throws, the handler reports the error and answers 500. The app calls its
// own route once with fetch (inside a "checkout client" span), flushes and
// prints one JSON line:
// `{"status": 500, "event_id", "trace_id", "server_span_id", "charge_span_id"}`
// with the ids the SDK used for the incoming request.

import * as Sentry from "@sentry/node";

const [traceLifecycle = "stream", tag = "run"] = process.argv.slice(2);

Sentry.init({
  dsn: process.env.DSN,
  release: "tracing-app@1.0.0",
  environment: "e2e",
  tracesSampleRate: 1,
  traceLifecycle,
  sendClientReports: false,
  shutdownTimeout: 10_000,
});

// Loaded after init so the SDK's http instrumentation sees the server.
const http = await import("node:http");

const seen = {};

function chargeCard(amount) {
  // A named callback: the SDK's own frames fill most of the 10-frame stack.
  return Sentry.startSpan({ name: "charge card", op: "payment.charge" }, function declineCard() {
    seen.charge_span_id = Sentry.getActiveSpan()?.spanContext().spanId;
    if (amount > 100) throw new Error(`card declined ${tag}`);
  });
}

const server = http.createServer(async (req, res) => {
  if (req.url !== "/checkout") {
    res.writeHead(404).end();
    return;
  }
  const span = Sentry.getActiveSpan();
  if (span) {
    const { traceId, spanId } = span.spanContext();
    Object.assign(seen, { trace_id: traceId, server_span_id: spanId });
  }
  try {
    await Sentry.startSpan({ name: "load cart", op: "db.query" }, () => new Promise((r) => setTimeout(r, 5)));
    chargeCard(250);
    res.writeHead(200).end("ok");
  } catch (e) {
    seen.event_id = Sentry.captureException(e);
    res.writeHead(500).end("checkout failed");
  }
});

await new Promise((r) => server.listen(0, "127.0.0.1", r));
const { port } = server.address();
// The caller is traced too (like a frontend calling an API), so the trace
// starts sampled here and continues in the server through `sentry-trace`.
const r = await Sentry.startSpan({ name: "checkout client", op: "function" }, async () => {
  const res = await fetch(`http://127.0.0.1:${port}/checkout`);
  await res.text();
  return res;
});
await new Promise((r) => server.close(r));
await Sentry.close(10_000);
console.log(JSON.stringify({ status: r.status, ...seen }));
