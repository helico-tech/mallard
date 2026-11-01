// The most important test: a minified Vite + TypeScript app in a real
// browser, sourcemaps uploaded with the real Rust sentry-cli, and the
// stored event symbolicated back to the original TypeScript.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import path from "node:path";
import { TestServer } from "../lib/server.mjs";
import {
  buildApp,
  clickCheckout,
  distDir,
  EXPECTED_FRAMES,
  launchBrowser,
  sentryCli,
  serveStatic,
  thrownFrames,
  thrownRawFrames,
  uploadSourcemaps,
} from "../lib/browser.mjs";

const TITLE = "TypeError: Cannot read properties of undefined (reading 'rate')";


let server;
let browser;
const cleanup = [];

before(async () => {
  [server, browser] = await Promise.all([TestServer.start("browser"), launchBrowser()]);
});

after(async () => {
  await browser?.close();
  for (const fn of cleanup.reverse()) await fn();
  await server?.stop();
});

/** Build (optionally inject + upload), serve on its own origin, return the url. */
async function deploy(name, { buildId, inject = true, upload }) {
  const dir = distDir(`sourcemaps-${name}`);
  cleanup.push(() => rm(dir, { recursive: true, force: true }));
  await buildApp({ outDir: dir, buildId });
  if (inject) {
    const out = await sentryCli(["sourcemaps", "inject", dir]);
    assert.match(out, /Modified: The following source files have been modified to have debug ids/);
  }
  if (upload) await upload(dir);
  const site = await serveStatic(dir);
  cleanup.push(site.close);
  assert.notEqual(new URL(site.url).origin, new URL(server.url).origin, "app must be on another origin");
  return { dir, url: site.url };
}

/** Click in Chromium and return the stored event for this build. */
async function crash(project, appUrl, { release, buildId }) {
  const r = await clickCheckout(browser, appUrl, { dsn: project.dsn, release });
  assert.deepEqual(r.pageErrors, ["Cannot read properties of undefined (reading 'rate')"]);
  assert.equal(r.flushed, true);
  assert.ok(r.requests.length >= 1, "the SDK must have sent an envelope");
  for (const req of r.requests) {
    assert.equal(req.status, 200, `envelope request failed (CORS?): ${JSON.stringify(req)}`);
    assert.equal(new URL(req.url).origin, new URL(server.url).origin);
  }
  return server.waitForEvent(
    project.slug,
    (e) => e.title === TITLE && Object.fromEntries(e.tags).build === buildId,
  );
}

function assertSymbolicated(ev, { resolvedWith, appUrl }) {
  const frames = thrownFrames(ev);
  const inApp = frames.filter((f) => f.in_app);
  assert.deepEqual(
    inApp.map((f) => [f.function, f.filename, f.lineno, f.context_line]),
    EXPECTED_FRAMES.map((e) => [e.function, e.filename, e.lineno, e.context]),
    `symbolicated in-app frames:\n${JSON.stringify(frames, null, 1)}`,
  );
  for (const f of inApp) {
    assert.equal(f.abs_path, `${appUrl}${f.filename}`);
    assert.equal(f.data.symbolicated, true);
    assert.equal(f.data.resolved_with, resolvedWith);
    assert.ok(f.colno > 0);
    assert.ok(f.pre_context.length > 0 && Array.isArray(f.post_context));
  }
  const top = inApp.at(-1);
  assert.deepEqual(top.pre_context.slice(-2), ["  }", "  // Bug: unknown coupons have no discount entry."]);
  assert.equal(top.post_context[0], "  return Math.round(subtotal * (1 - Number(rate)));");
  // The SDK's own wrapper frame resolves into node_modules and is not in-app.
  const sdkFrame = frames.find((f) => f.filename.includes("/node_modules/@sentry/"));
  assert.ok(sdkFrame, "expected the SDK's addEventListener wrapper frame");
  assert.equal(sdkFrame.in_app, false);

  // raw_stacktrace keeps the minified frames.
  const raw = thrownRawFrames(ev);
  assert.equal(raw.length, frames.length);
  for (const f of raw) {
    assert.match(f.abs_path, new RegExp(`^${appUrl}/assets/index-[\\w-]+\\.js$`));
    assert.equal(f.lineno < 100, true);
    assert.ok(f.colno > 100, "minified column");
    assert.equal(f.data?.symbolicated, undefined);
  }
  assert.ok(!raw.some((f) => EXPECTED_FRAMES.some((e) => e.function === f.function)), "raw frames keep minified names");

  assert.equal(ev.culprit, "applyDiscount(/src/pricing.ts)");
}

test("debug ids: inject + upload with sentry-cli, symbolicate a browser crash", async (t) => {
  assert.match(await sentryCli(["--version"]), /sentry-cli 3\./);
  const project = await server.createProject("Web Debug Ids");
  const upload = async (dir) => {
    const out = await uploadSourcemaps(server, project.slug, dir);
    assert.match(out, /File processing complete/);
    assert.match(out, /Upload type: artifact bundle/);
  };
  const a = await deploy("debugid-a", { buildId: "a", upload });
  const bundles = await server.sourcemaps(project.slug);
  assert.equal(bundles.length, 1);
  assert.equal(bundles[0].file_count, 2);
  assert.equal(bundles[0].release, null);
  assert.equal(bundles[0].debug_ids.length, 1);

  const ev = await crash(project, a.url, { buildId: "a" });
  assertSymbolicated(ev, { resolvedWith: "debug-id", appUrl: a.url });
  assert.equal(ev.payload.debug_meta.images[0].debug_id, bundles[0].debug_ids[0]);
  t.diagnostic(`symbolicated top frame: ${JSON.stringify(thrownFrames(ev).at(-1))}`);

  // Re-uploading the same files is a no-op for the CLI.
  const again = await uploadSourcemaps(server, project.slug, a.dir);
  assert.match(again, /Nothing to upload, all files are on the server|File processing complete/);
  assert.equal((await server.sourcemaps(project.slug)).length, 1);

  await t.test("a rebuild with another bundle hash groups into the same issue", async () => {
    const b = await deploy("debugid-b", { buildId: "b", upload });
    const rawA = thrownRawFrames(ev)[0].abs_path;
    const ev2 = await crash(project, b.url, { buildId: "b" });
    const rawB = thrownRawFrames(ev2)[0].abs_path;
    assert.notEqual(path.basename(rawA), path.basename(rawB), "bundle file names must differ");
    assertSymbolicated(ev2, { resolvedWith: "debug-id", appUrl: b.url });
    assert.equal(ev2.issue_id, ev.issue_id);
    const issue = await server.issue(project.slug, ev.issue_id);
    assert.equal(issue.count, 2);
    assert.equal(issue.title, TITLE);
    assert.equal((await server.sourcemaps(project.slug)).length, 2);
  });
});

test("debug ids + --release: bundle is tagged with the release", async () => {
  const project = await server.createProject("Web Debug Ids Release");
  const d = await deploy("debugid-release", {
    buildId: "r",
    upload: (dir) => uploadSourcemaps(server, project.slug, dir, ["--release", "web@1.0.0"]),
  });
  const [bundle] = await server.sourcemaps(project.slug);
  assert.equal(bundle.release, "web@1.0.0");
  const ev = await crash(project, d.url, { buildId: "r", release: "web@1.0.0" });
  assert.equal(ev.release, "web@1.0.0");
  assertSymbolicated(ev, { resolvedWith: "debug-id", appUrl: d.url });
});

test("release only (no debug ids): upload --release, match by URL", async () => {
  const project = await server.createProject("Web Release Only");
  const d = await deploy("release-only", {
    buildId: "ro",
    inject: false,
    upload: async (dir) => {
      const out = await uploadSourcemaps(server, project.slug, dir, ["--release", "web@2.0.0"]);
      assert.match(out, /~\/assets\/index-[\w-]+\.js /);
    },
  });
  const [bundle] = await server.sourcemaps(project.slug);
  assert.equal(bundle.release, "web@2.0.0");
  assert.deepEqual(bundle.debug_ids, []);
  const ev = await crash(project, d.url, { buildId: "ro", release: "web@2.0.0" });
  assert.equal(ev.payload.debug_meta, undefined, "no debug ids were injected");
  assertSymbolicated(ev, { resolvedWith: "release", appUrl: d.url });
});

test("release only with --url-prefix '~/assets'", async () => {
  const project = await server.createProject("Web Url Prefix");
  const d = await deploy("url-prefix", {
    buildId: "up",
    inject: false,
    upload: async (dir) => {
      const out = await uploadSourcemaps(server, project.slug, path.join(dir, "assets"), [
        "--release",
        "web@3.0.0",
        "--url-prefix",
        "~/assets",
      ]);
      assert.match(out, /~\/assets\/index-[\w-]+\.js /);
    },
  });
  const ev = await crash(project, d.url, { buildId: "up", release: "web@3.0.0" });
  assertSymbolicated(ev, { resolvedWith: "release", appUrl: d.url });
});

test("a different release does not use the uploaded maps", async () => {
  const project = await server.createProject("Web Wrong Release");
  const d = await deploy("wrong-release", {
    buildId: "wr",
    inject: false,
    upload: (dir) => uploadSourcemaps(server, project.slug, dir, ["--release", "web@4.0.0"]),
  });
  const ev = await crash(project, d.url, { buildId: "wr", release: "web@9.9.9" });
  const frames = thrownFrames(ev);
  assert.ok(frames.every((f) => f.data?.symbolicated === undefined));
  assert.equal(ev.payload.exception.values.at(-1).raw_stacktrace, undefined);
});

// sentry-cli 2.x reads the server's `accept` capabilities (3.x ignores
// them), so it takes a slightly different path through the protocol.
test("sentry-cli 2.x: inject + upload (debug ids) and release-only upload", async () => {
  const project = await server.createProject("Web Cli Two");
  const dir = distDir("sourcemaps-cli2");
  cleanup.push(() => rm(dir, { recursive: true, force: true }));
  await buildApp({ outDir: dir, buildId: "v2" });
  const version = await sentryCli(["--version"], { major: 2 });
  assert.match(version, /sentry-cli 2\./);
  await sentryCli(["sourcemaps", "inject", dir], { major: 2 });
  const out = await uploadSourcemaps(server, project.slug, dir, ["--release", "web@5.0.0"], { major: 2 });
  assert.match(out, /Upload type: artifact bundle/);
  assert.match(out, /File processing complete/);
  const site = await serveStatic(dir);
  cleanup.push(site.close);
  const ev = await crash(project, site.url, { buildId: "v2", release: "web@5.0.0" });
  assertSymbolicated(ev, { resolvedWith: "debug-id", appUrl: site.url });

  const project2 = await server.createProject("Web Cli Two Release");
  const d = await deploy("cli2-release-only", {
    buildId: "v2r",
    inject: false,
    upload: (dir2) => uploadSourcemaps(server, project2.slug, dir2, ["--release", "web@6.0.0"], { major: 2 }),
  });
  const ev2 = await crash(project2, d.url, { buildId: "v2r", release: "web@6.0.0" });
  assertSymbolicated(ev2, { resolvedWith: "release", appUrl: d.url });
});
