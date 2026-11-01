// @sentry/node against mallard: only the DSN changes.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { TestServer, exceptions, messageOf } from "../lib/server.mjs";
import { runNodeApp } from "../lib/node.mjs";

let server;
let project;

before(async () => {
  server = await TestServer.start("node");
  project = await server.createProject("Node App");
  assert.match(project.dsn, /^http:\/\/[0-9a-f]{32}@127\.0\.0\.1:\d+\/sentry\/\d+$/);
});

after(async () => {
  await server?.stop();
});

async function run(scenario, tag) {
  const r = await runNodeApp(project.dsn, scenario, tag);
  assert.equal(r.code, 0, `node-app ${scenario} failed:\n${r.stderr}`);
  return r.ids;
}

const event = (id) => server.event(project.slug, id);

test("captureException with a nested cause chain", async () => {
  const { cause } = await run("cause_chain");
  const ev = await event(cause);
  assert.equal(ev.title, "Error: app failed to start");
  assert.equal(ev.release, "node-app@1.2.3");
  assert.equal(ev.environment, "e2e");
  assert.equal(ev.platform, "node");
  const ex = exceptions(ev);
  // Sentry order: root cause first, the thrown error last.
  assert.deepEqual(
    ex.map((e) => [e.type, e.value]),
    [
      ["TypeError", "Cannot read properties of undefined (reading 'toUpperCase')"],
      ["TypeError", "could not load settings"],
      ["Error", "app failed to start"],
    ],
  );
  assert.equal(ex[0].mechanism.source, "cause");
  const fns = ex[0].stacktrace.frames.map((f) => f.function);
  assert.equal(fns.at(-1), "parseConfig");
  assert.ok(fns.includes("loadSettings"));
  const top = ex[0].stacktrace.frames.at(-1);
  assert.match(top.filename, /node-app\.mjs$/);
  assert.equal(top.in_app, true);
  assert.ok(top.lineno > 0);
  assert.match(top.context_line, /JSON\.parse\(input\)\.settings\.theme\.toUpperCase\(\)/);

  const [issue] = await server.issues(project.slug, { query: "app failed to start" });
  assert.equal(issue.id, ev.issue_id);
  assert.equal(issue.exception_type, "Error");
  assert.equal(issue.culprit, "startApp(node-app)");
});

test("captureMessage with a level", async () => {
  const { message } = await run("message", "m1");
  const ev = await event(message);
  assert.equal(ev.level, "warning");
  assert.equal(messageOf(ev), "disk almost full m1");
  assert.equal(ev.title, "disk almost full m1");
  const issue = await server.issue(project.slug, ev.issue_id);
  assert.equal(issue.level, "warning");
  assert.equal(issue.exception_type, null);
});

test("breadcrumbs, setUser, setTag, setContext", async () => {
  const { scope } = await run("scope", "s1");
  const ev = await event(scope);
  const p = ev.payload;
  assert.deepEqual(p.user, { id: "user-42", email: "jane@example.com", username: "jane" });
  assert.equal(ev.user, "id:user-42");
  assert.deepEqual(p.tags, { tenant: "acme", plan: "pro" });
  const tags = Object.fromEntries(ev.tags);
  assert.equal(tags.tenant, "acme");
  assert.equal(tags.plan, "pro");
  assert.equal(tags.release, "node-app@1.2.3");
  assert.equal(tags.environment, "e2e");
  assert.deepEqual(p.contexts.order, { id: "ord-1001", total: 99.95, items: 3 });
  assert.equal(p.contexts.runtime.name, "node");
  const crumbs = p.breadcrumbs.values.filter((b) => b.category === "auth" || b.category === "cart");
  assert.deepEqual(
    crumbs.map((b) => [b.category, b.message, b.level]),
    [
      ["auth", "user logged in", "info"],
      ["cart", "added item sku-42", "debug"],
    ],
  );
  assert.deepEqual(crumbs[0].data, { method: "password" });

  const issue = await server.issue(project.slug, ev.issue_id);
  assert.equal(issue.user_count, 1);
  const tenant = issue.tags.find((t) => t.key === "tenant");
  assert.deepEqual(tenant.top, [{ value: "acme", count: 1 }]);
});

test("custom fingerprint groups different errors; {{ default }} splits one", async () => {
  const ids = await run("fingerprint");
  const [a, b, ta, tb] = await Promise.all(
    [ids.fp_a, ids.fp_b, ids["split_tenant-a"], ids["split_tenant-b"]].map(event),
  );
  assert.equal(a.payload.exception.values[0].type, "Error");
  assert.equal(b.payload.exception.values[0].type, "RangeError");
  assert.deepEqual(a.payload.fingerprint, ["payment-failure"]);
  assert.equal(a.issue_id, b.issue_id, "fingerprint must group two different errors");
  const grouped = await server.issue(project.slug, a.issue_id);
  assert.equal(grouped.count, 2);

  assert.equal(ta.title, tb.title);
  assert.notEqual(ta.issue_id, tb.issue_id, "{{ default }} + tenant must split per tenant");
  assert.notEqual(ta.issue_id, a.issue_id);
});

test("the same error thrown 3 times is one issue with count 3", async () => {
  const ids = [];
  for (let i = 0; i < 3; i++) ids.push((await run("repeat", "r1")).repeat);
  assert.equal(new Set(ids).size, 3);
  const evs = await Promise.all(ids.map(event));
  const issueIds = new Set(evs.map((e) => e.issue_id));
  assert.equal(issueIds.size, 1);
  const issue = await server.issue(project.slug, evs[0].issue_id);
  assert.equal(issue.count, 3);
  assert.equal(issue.title, "Error: repeated failure r1");
  assert.equal(issue.first_release, "node-app@1.2.3");
  const events = await server.events(project.slug, issue.id);
  assert.deepEqual(new Set(events.map((e) => e.event_id)), new Set(ids));
});

test("resolve then re-throw marks the issue unresolved (regression)", async () => {
  const first = (await run("regression", "g1")).regression;
  const ev = await event(first);
  const resolved = await server.setStatus(project.slug, ev.issue_id, "resolved");
  assert.equal(resolved.status, "resolved");
  assert.equal((await server.issue(project.slug, ev.issue_id)).status, "resolved");

  const second = (await run("regression", "g1")).regression;
  const ev2 = await event(second);
  assert.equal(ev2.issue_id, ev.issue_id);
  const issue = await server.issue(project.slug, ev.issue_id);
  assert.equal(issue.status, "unresolved");
  assert.equal(issue.count, 2);
  const reasons = issue.activity.map((a) => [a.status, a.reason]);
  assert.deepEqual(reasons.find((r) => r[0] === "resolved"), ["resolved", "user"]);
  assert.deepEqual(reasons.find((r) => r[1] === "regression"), ["unresolved", "regression"]);
});

test("events captured inside a span carry its trace_id", async () => {
  const ids = await run("span", "t1");
  assert.match(ids.trace_id, /^[0-9a-f]{32}$/);
  const ev = await event(ids.span);
  assert.equal(ev.trace_id, ids.trace_id);
  assert.match(ev.span_id, /^[0-9a-f]{16}$/);
  assert.equal(ev.payload.contexts.trace.trace_id, ids.trace_id);
  // Cross-project and per-project lookups by trace id.
  const byTrace = await server.api("GET", `/v1/projects/${project.slug}/events?trace_id=${ids.trace_id}`);
  assert.deepEqual(byTrace.events.map((e) => e.event_id), [ids.span]);
  const global = await server.api("GET", `/v1/events?trace_id=${ids.trace_id}`);
  assert.deepEqual(global.events.map((e) => [e.event_id, e.project_id]), [[ids.span, project.id]]);
});

test("uncaught exception in a child process is flushed before exit", async () => {
  const r = await runNodeApp(project.dsn, "uncaught", "u1");
  assert.equal(r.code, 1, "the process must still crash");
  assert.match(r.stderr, /uncaught boom u1/);
  const ev = await server.waitForEvent(project.slug, (e) => e.title === "Error: uncaught boom u1");
  assert.equal(ev.level, "fatal");
  const ex = exceptions(ev).at(-1);
  assert.deepEqual(ex.mechanism, { handled: false, type: "auto.node.onuncaughtexception" });
  assert.match(ex.stacktrace.frames.at(-1).function, /crashOnTimer/);
  assert.equal(Object.fromEntries(ev.tags).handled, "no");
});

test("unhandled rejection in a child process is flushed before exit", async () => {
  const r = await runNodeApp(project.dsn, "unhandled_rejection", "p1");
  assert.equal(r.code, 1);
  const ev = await server.waitForEvent(project.slug, (e) => e.title === "Error: unhandled rejection p1");
  assert.equal(ev.level, "fatal");
  const ex = exceptions(ev).at(-1);
  assert.deepEqual(ex.mechanism, { handled: false, type: "auto.node.onunhandledrejection" });
  assert.match(ex.stacktrace.frames.at(-1).function, /rejectLater/);
  assert.equal(ev.payload.extra.unhandledPromiseRejection, true);
});
