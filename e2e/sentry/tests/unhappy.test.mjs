// Error paths and older transports: bad keys, unknown projects, gzip
// envelopes, the legacy /store/ endpoint, CORS preflight.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { gzipSync, deflateSync } from "node:zlib";
import { TestServer } from "../lib/server.mjs";
import { runNodeApp } from "../lib/node.mjs";

let server;
let project;
let key;

before(async () => {
  server = await TestServer.start("unhappy");
  project = await server.createProject("Unhappy");
  key = project.public_key;
});

after(async () => {
  await server?.stop();
});

const auth = (k = key) => `Sentry sentry_version=7, sentry_key=${k}, sentry_client=e2e-raw/1.0`;
const eventId = () => randomUUID().replaceAll("-", "");

function envelope(id, event) {
  return [
    JSON.stringify({ event_id: id, sent_at: new Date().toISOString() }),
    JSON.stringify({ type: "event" }),
    JSON.stringify({ event_id: id, ...event }),
  ].join("\n");
}

function post(path, body, headers = {}) {
  return fetch(`${server.url}${path}`, { method: "POST", body, headers });
}

test("wrong DSN key: 401 with a Sentry error body, and the SDK does not crash", async () => {
  const id = eventId();
  const r = await post(`/api/${project.id}/envelope/`, envelope(id, { message: "nope" }), {
    "x-sentry-auth": auth("0".repeat(32)),
  });
  assert.equal(r.status, 401);
  assert.equal(r.headers.get("x-sentry-error"), "invalid api key");
  assert.deepEqual(await r.json(), { detail: "invalid api key" });

  const missing = await post(`/api/${project.id}/envelope/`, envelope(eventId(), { message: "nope" }));
  assert.equal(missing.status, 401);

  const badDsn = project.dsn.replace(key, "f".repeat(32));
  const run = await runNodeApp(badDsn, "message", "wrong-key");
  assert.equal(run.code, 0, `SDK crashed:\n${run.stderr}`);
  assert.ok(run.ids.message, "SDK still returns an event id");
  const events = await server.events(project.slug);
  assert.ok(!events.some((e) => e.event_id === id || e.event_id === run.ids.message));
});

test("unknown project: 404", async () => {
  const r = await post(`/api/999999/envelope/`, envelope(eventId(), { message: "x" }), { "x-sentry-auth": auth() });
  assert.equal(r.status, 404);
  assert.equal(r.headers.get("x-sentry-error"), "project not found");

  const run = await runNodeApp(project.dsn.replace(/\/\d+$/, "/999999"), "message", "unknown-project");
  assert.equal(run.code, 0, `SDK crashed:\n${run.stderr}`);
});

test("gzip-compressed envelope (Content-Encoding: gzip)", async () => {
  const id = eventId();
  const body = gzipSync(
    envelope(id, {
      platform: "python",
      level: "error",
      release: "raw@1.0",
      exception: { values: [{ type: "ValueError", value: "gzip works", stacktrace: { frames: [{ function: "main", filename: "app.py", lineno: 3, in_app: true }] } }] },
    }),
  );
  const r = await post(`/api/${project.id}/envelope/`, body, {
    "x-sentry-auth": auth(),
    "content-encoding": "gzip",
    "content-type": "application/x-sentry-envelope",
  });
  assert.equal(r.status, 200, await r.clone().text());
  assert.deepEqual(await r.json(), { id });
  const ev = await server.event(project.slug, id);
  assert.equal(ev.title, "ValueError: gzip works");
  assert.equal(ev.release, "raw@1.0");
  assert.equal(ev.platform, "python");
});

test("corrupt gzip body is rejected with 400", async () => {
  const r = await post(`/api/${project.id}/envelope/`, Buffer.from("not gzip at all"), {
    "x-sentry-auth": auth(),
    "content-encoding": "gzip",
  });
  assert.equal(r.status, 400);
});

test("auth via query string and envelope header DSN", async () => {
  const id = eventId();
  const r = await post(`/api/${project.id}/envelope/?sentry_key=${key}&sentry_version=7`, envelope(id, { message: "query auth" }));
  assert.equal(r.status, 200);
  assert.equal((await server.event(project.slug, id)).message, "query auth");

  const id2 = eventId();
  const body = [
    JSON.stringify({ event_id: id2, dsn: project.dsn }),
    JSON.stringify({ type: "event" }),
    JSON.stringify({ event_id: id2, message: "dsn header auth" }),
  ].join("\n");
  const r2 = await post(`/api/${project.id}/envelope/`, body);
  assert.equal(r2.status, 200);
  assert.equal((await server.event(project.slug, id2)).message, "dsn header auth");
});

test("legacy /store/ endpoint with a JSON body", async () => {
  const id = eventId();
  const r = await post(
    `/api/${project.id}/store/`,
    JSON.stringify({
      event_id: id,
      platform: "javascript",
      level: "warning",
      message: "legacy store event",
      tags: { legacy: "yes" },
      user: { id: "u-1" },
    }),
    { "x-sentry-auth": auth(), "content-type": "application/json" },
  );
  assert.equal(r.status, 200);
  assert.deepEqual(await r.json(), { id });
  const ev = await server.event(project.slug, id);
  assert.equal(ev.message, "legacy store event");
  assert.equal(ev.level, "warning");
  assert.equal(ev.user, "id:u-1");
  assert.equal(Object.fromEntries(ev.tags).legacy, "yes");
});

test("legacy /store/ with a base64 + zlib body (very old SDKs)", async () => {
  const id = eventId();
  const body = deflateSync(JSON.stringify({ event_id: id, message: "zlib store event" })).toString("base64");
  const r = await post(`/api/${project.id}/store/?sentry_key=${key}`, body);
  assert.equal(r.status, 200);
  assert.equal((await server.event(project.slug, id)).message, "zlib store event");
});

test("CORS preflight from a browser origin", async () => {
  const r = await fetch(`${server.url}/api/${project.id}/envelope/`, {
    method: "OPTIONS",
    headers: {
      origin: "http://app.example.test",
      "access-control-request-method": "POST",
      "access-control-request-headers": "content-type,x-sentry-auth",
    },
  });
  assert.ok(r.status === 200 || r.status === 204, `preflight status ${r.status}`);
  assert.equal(r.headers.get("access-control-allow-origin"), "http://app.example.test");
  assert.match(r.headers.get("access-control-allow-methods"), /POST/);
  assert.match(r.headers.get("access-control-allow-headers"), /x-sentry-auth/i);
});

test("management API requires the bearer token", async () => {
  const r = await fetch(`${server.url}/v1/projects`);
  assert.equal(r.status, 401);
  const r2 = await fetch(`${server.url}/api/0/organizations/default/chunk-upload/`, { headers: { authorization: "Bearer wrong" } });
  assert.equal(r2.status, 401);
  await assert.rejects(server.api("GET", "/v1/projects/does-not-exist"), (e) => e.status === 404);
});
