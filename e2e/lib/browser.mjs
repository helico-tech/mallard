// Build the Vite + TypeScript app in browser-app/, serve it on its own
// origin, drive it with Playwright Chromium, and run the real sentry-cli.

import { execFile } from "node:child_process";
import { createReadStream, readFileSync } from "node:fs";
import { stat } from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { promisify } from "node:util";
import { createRequire } from "node:module";
import { build } from "vite";
import { chromium } from "playwright";
import { E2E_DIR } from "./server.mjs";

export const APP_DIR = path.join(E2E_DIR, "browser-app");
const require = createRequire(import.meta.url);

/**
 * Path of the Rust sentry-cli binary. `@sentry/cli` is 3.x; `sentry-cli-v2`
 * is an npm alias of `@sentry/cli@2` (both ship a `sentry-cli` bin, so we
 * resolve each package's own binary instead of node_modules/.bin).
 */
export function sentryCliPath(major = 3) {
  const pkg = major === 2 ? "sentry-cli-v2" : "@sentry/cli";
  return require(`${pkg}/js/helper.js`).getPath();
}

/**
 * Output dir for a build, inside browser-app/ like a real project's dist/
 * (so map `sources` are `../../src/x.ts`). Git-ignored.
 */
export function distDir(name) {
  return path.join(APP_DIR, ".dist", name);
}

/** Production build with sourcemaps. `buildId` changes the bundle hash. */
export async function buildApp({ outDir, buildId, plugins = [], sourcemap = true }) {
  await build({
    root: APP_DIR,
    configFile: false,
    logLevel: "warn",
    plugins,
    define: { __BUILD_ID__: JSON.stringify(buildId) },
    build: { outDir, emptyOutDir: true, sourcemap, minify: true },
  });
}

const TYPES = { ".html": "text/html", ".js": "text/javascript", ".map": "application/json", ".css": "text/css" };

/** Serve `dir` on a fresh 127.0.0.1 port. Returns `{ url, close }`. */
export async function serveStatic(dir) {
  const srv = http.createServer(async (req, res) => {
    const p = decodeURIComponent(new URL(req.url, "http://x").pathname);
    const file = path.join(dir, p === "/" ? "index.html" : p);
    if (!file.startsWith(dir)) return res.writeHead(403).end();
    try {
      if (!(await stat(file)).isFile()) throw new Error("not a file");
    } catch {
      return res.writeHead(404).end("not found");
    }
    res.writeHead(200, { "content-type": TYPES[path.extname(file)] ?? "application/octet-stream" });
    createReadStream(file).pipe(res);
  });
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  const { port } = srv.address();
  if (port === 8765) {
    srv.close();
    return serveStatic(dir);
  }
  return {
    url: `http://127.0.0.1:${port}`,
    close: () => new Promise((r) => srv.close(r)),
  };
}

export async function launchBrowser() {
  return chromium.launch({ headless: true });
}

/**
 * Open the app with `dsn`/`release`, click the checkout button (which
 * throws), flush the SDK and return what the page saw.
 */
export async function clickCheckout(browser, appUrl, { dsn, release }) {
  const page = await browser.newPage();
  const requests = [];
  const pageErrors = [];
  const consoleErrors = [];
  page.on("pageerror", (e) => pageErrors.push(e.message));
  page.on("console", (m) => m.type() === "error" && consoleErrors.push(m.text()));
  page.on("response", (r) => {
    if (r.url().includes("/envelope/")) requests.push({ url: r.url(), status: r.status(), method: r.request().method() });
  });
  const qs = new URLSearchParams({ dsn, ...(release ? { release } : {}) });
  await page.goto(`${appUrl}/?${qs}`);
  await page.waitForFunction(() => document.getElementById("status")?.textContent === "ready");
  await page.click("#checkout");
  const flushed = await page.evaluate(() => window.sdlFlush());
  await page.close();
  return { flushed, requests, pageErrors, consoleErrors };
}

/** Run the npm-installed Rust sentry-cli. Resolves with stdout+stderr. */
export async function sentryCli(args, { cwd = E2E_DIR, major = 3 } = {}) {
  try {
    const { stdout, stderr } = await promisify(execFile)(sentryCliPath(major), args, {
      cwd,
      env: { ...process.env, SENTRY_LOG_LEVEL: "info", SENTRY_DISABLE_UPDATE_CHECK: "1" },
      maxBuffer: 16 * 1024 * 1024,
    });
    return stdout + stderr;
  } catch (e) {
    throw new Error(`sentry-cli ${args.join(" ")} failed (${e.code}):\n${e.stdout}\n${e.stderr}`);
  }
}

/** `sentry-cli sourcemaps upload` against the test server. */
export function uploadSourcemaps(server, slug, dir, extra = [], { major = 3 } = {}) {
  const args = ["--url", server.url, "--auth-token", server.token, "sourcemaps", "upload"];
  args.push("--org", "default", "--project", slug, ...extra, dir);
  return sentryCli(args, { major });
}

/** The thrown exception's frames (innermost last). */
export function thrownFrames(detail) {
  return detail.payload.exception.values.at(-1).stacktrace.frames;
}

export function thrownRawFrames(detail) {
  return detail.payload.exception.values.at(-1).raw_stacktrace?.frames;
}

/** 1-based line of `text` in browser-app/src/<file>, so tests survive edits. */
function lineOf(file, text) {
  const lines = readFileSync(path.join(APP_DIR, "src", file), "utf8").split("\n");
  const i = lines.findIndex((l) => l === text);
  if (i < 0) throw new Error(`${text} not found in ${file}`);
  return i + 1;
}

const frame = (fn, file, context) => ({ function: fn, filename: `/src/${file}`, lineno: lineOf(file, context), context });

/** The in-app frames (outermost first) a symbolicated checkout crash must have. */
export const EXPECTED_FRAMES = [
  frame(
    "HTMLButtonElement.handleCheckoutClick",
    "main.ts",
    '  const order = submitOrder({ items: [{ sku: "sku-42", price: 1999, qty: 2 }], coupon: params.get("coupon") ?? "NONE" });',
  ),
  frame("submitOrder", "checkout.ts", "  const total = computeTotal(cart);"),
  frame("computeTotal", "cart.ts", "  return applyDiscount(subtotal, cart.coupon);"),
  frame("applyDiscount", "pricing.ts", "  const rate = (discount as Discount).rate.toFixed(2);"),
];
