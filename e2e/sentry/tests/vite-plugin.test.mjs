// @sentry/vite-plugin (which uploads with the new TypeScript `sentry` CLI,
// not the Rust one) pointed at mallard via `url`.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { readdir, rm } from "node:fs/promises";
import path from "node:path";
import { sentryVitePlugin } from "@sentry/vite-plugin";
import { TestServer } from "../lib/server.mjs";
import { buildApp, clickCheckout, distDir, EXPECTED_FRAMES, launchBrowser, serveStatic, thrownFrames, thrownRawFrames } from "../lib/browser.mjs";

let server;
let browser;
let site;
const outDir = distDir("vite-plugin");

before(async () => {
  [server, browser] = await Promise.all([TestServer.start("vite-plugin"), launchBrowser()]);
});

after(async () => {
  await browser?.close();
  await site?.close();
  await rm(outDir, { recursive: true, force: true });
  await server?.stop();
});

test("vite plugin uploads sourcemaps and injects the release", async (t) => {
  const project = await server.createProject("Web Vite Plugin");
  const errors = [];
  // A typical user config: no build.sourcemap (the plugin turns on hidden
  // sourcemaps) and the maps are deleted after upload, so the server can
  // only use what was uploaded.
  await buildApp({
    outDir,
    buildId: "plugin",
    sourcemap: undefined,
    plugins: [
      sentryVitePlugin({
        url: server.url,
        authToken: server.token,
        org: "default",
        project: project.slug,
        release: { name: "plugin-app@1.0.0" },
        telemetry: false,
        // An absolute glob: relative ones resolve against the cwd.
        sourcemaps: { filesToDeleteAfterUpload: [path.join(outDir, "**/*.map")] },
        errorHandler: (e) => errors.push(e),
      }),
    ],
  });
  assert.deepEqual(errors.map((e) => e.message), [], "plugin reported errors");
  const assets = await readdir(path.join(outDir, "assets"));
  assert.ok(!assets.some((f) => f.endsWith(".map")), `maps should be deleted after upload: ${assets}`);

  const [bundle] = await server.sourcemaps(project.slug);
  assert.ok(bundle, "plugin must have uploaded a bundle");
  assert.equal(bundle.release, "plugin-app@1.0.0");
  assert.equal(bundle.debug_ids.length, 1);

  site = await serveStatic(outDir);
  // No release in the URL: the plugin injected it into the bundle.
  const r = await clickCheckout(browser, site.url, { dsn: project.dsn });
  assert.equal(r.flushed, true);
  const ev = await server.waitForEvent(project.slug, (e) => e.title.startsWith("TypeError"));
  assert.equal(ev.release, "plugin-app@1.0.0");
  assert.equal(ev.payload.debug_meta.images[0].debug_id, bundle.debug_ids[0]);

  const inApp = thrownFrames(ev).filter((f) => f.in_app);
  assert.deepEqual(
    inApp.map((f) => [f.function, f.filename, f.lineno]),
    EXPECTED_FRAMES.map((e) => [e.function, e.filename, e.lineno]),
  );
  const top = inApp.at(-1);
  assert.equal(top.context_line, "  const rate = (discount as Discount).rate.toFixed(2);");
  assert.equal(top.data.resolved_with, "debug-id");
  assert.ok(thrownRawFrames(ev).every((f) => /\/assets\/index-[\w-]+\.js$/.test(f.abs_path)));
  t.diagnostic(`top frame: ${JSON.stringify({ ...top, pre_context: undefined, post_context: undefined })}`);
  assert.ok(existsSync(outDir));
});
