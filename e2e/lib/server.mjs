// Test harness: start one sentry-ducklake server per test file on a free
// port with its own temp dir, and talk to its /v1 API.
//
// Binary: $SDL_BIN, else ../target/debug/sentry-ducklake (built with
// `cargo build -p sdl-server` if missing).

import { spawn, execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const E2E_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const REPO_DIR = path.resolve(E2E_DIR, "..");
const TOKEN = "e2e-token";

export function serverBinary() {
  if (process.env.SDL_BIN) return path.resolve(process.env.SDL_BIN);
  const bin = path.join(REPO_DIR, "target", "debug", "sentry-ducklake");
  if (!existsSync(bin)) {
    console.error(`[e2e] ${bin} missing, running cargo build -p sdl-server ...`);
    execFileSync("cargo", ["build", "-p", "sdl-server"], { cwd: REPO_DIR, stdio: "inherit" });
  }
  return bin;
}

/** A free TCP port on 127.0.0.1 (never 8765, the dev server's port). */
export async function freePort() {
  for (;;) {
    const port = await new Promise((resolve, reject) => {
      const s = net.createServer();
      s.unref();
      s.on("error", reject);
      s.listen(0, "127.0.0.1", () => {
        const { port } = s.address();
        s.close(() => resolve(port));
      });
    });
    if (port !== 8765) return port;
  }
}

export async function tempDir(prefix) {
  const base = process.env.SDL_E2E_TMPDIR || os.tmpdir();
  return mkdtemp(path.join(base, `sdl-e2e-${prefix}-`));
}

/** Poll `fn` until it returns a truthy value or the timeout passes. */
export async function waitFor(fn, { timeout = 30_000, interval = 100, what = "condition" } = {}) {
  const deadline = Date.now() + timeout;
  let last;
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
    } catch (e) {
      last = e;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out after ${timeout} ms waiting for ${what}${last ? `: ${last.message}` : ""}`);
    }
    await new Promise((r) => setTimeout(r, interval));
  }
}

export class TestServer {
  constructor({ url, port, dir, proc, logs }) {
    Object.assign(this, { url, port, dir, proc, logs, token: TOKEN });
  }

  /** Start a server. Remember to `await server.stop()` (use `after`). */
  static async start(name = "test") {
    const bin = serverBinary();
    const dir = await tempDir(name);
    const port = await freePort();
    const url = `http://127.0.0.1:${port}`;
    const logs = [];
    const proc = spawn(bin, [], {
      env: {
        ...process.env,
        SDL_API_TOKEN: TOKEN,
        SDL_STORAGE_URL: path.join(dir, "storage"),
        SDL_CATALOG: path.join(dir, "catalog.ducklake"),
        SDL_LISTEN: `127.0.0.1:${port}`,
        SDL_PUBLIC_URL: url,
        RUST_LOG: process.env.RUST_LOG || "info",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    for (const s of [proc.stdout, proc.stderr]) {
      s.setEncoding("utf8");
      s.on("data", (d) => {
        logs.push(d);
        if (process.env.SDL_E2E_VERBOSE) process.stderr.write(`[server ${port}] ${d}`);
      });
    }
    let exited = null;
    proc.on("exit", (code, signal) => (exited = { code, signal }));
    const server = new TestServer({ url, port, dir, proc, logs });
    try {
      await waitFor(
        async () => {
          if (exited) throw new Error(`server exited early (${JSON.stringify(exited)}):\n${logs.join("")}`);
          const r = await fetch(`${url}/readyz`);
          return r.ok;
        },
        { timeout: 60_000, what: "GET /readyz" },
      );
    } catch (e) {
      await server.stop();
      throw e;
    }
    return server;
  }

  async stop() {
    if (this.proc.exitCode === null && this.proc.signalCode === null) {
      const done = new Promise((r) => this.proc.once("exit", r));
      this.proc.kill("SIGTERM");
      const timer = setTimeout(() => this.proc.kill("SIGKILL"), 10_000);
      await done;
      clearTimeout(timer);
    }
    if (!process.env.SDL_E2E_KEEP) await rm(this.dir, { recursive: true, force: true });
  }

  /** Server log output so far (useful in assertion messages). */
  log() {
    return this.logs.join("");
  }

  async api(method, p, body) {
    const r = await fetch(`${this.url}${p}`, {
      method,
      headers: { authorization: `Bearer ${this.token}`, ...(body ? { "content-type": "application/json" } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    const text = await r.text();
    let json;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = text;
    }
    if (!r.ok) {
      const err = new Error(`${method} ${p} -> ${r.status}: ${text}`);
      err.status = r.status;
      err.body = json;
      throw err;
    }
    return json;
  }

  createProject(name) {
    return this.api("POST", "/v1/projects", { name });
  }

  issues(slug, query = {}) {
    const qs = new URLSearchParams({ status: "all", ...query });
    return this.api("GET", `/v1/projects/${slug}/issues?${qs}`).then((r) => r.issues);
  }

  issue(slug, id) {
    return this.api("GET", `/v1/projects/${slug}/issues/${id}`);
  }

  setStatus(slug, id, status) {
    return this.api("PATCH", `/v1/projects/${slug}/issues/${id}`, { status });
  }

  events(slug, issueId) {
    const qs = issueId ? `?issue=${issueId}&limit=500` : "?limit=500";
    return this.api("GET", `/v1/projects/${slug}/events${qs}`).then((r) => r.events);
  }

  event(slug, eventId, issueId) {
    const qs = issueId ? `?issue=${issueId}` : "";
    return this.api("GET", `/v1/projects/${slug}/events/${eventId}${qs}`);
  }

  sourcemaps(slug) {
    return this.api("GET", `/v1/projects/${slug}/sourcemaps`);
  }

  /** Full detail of every stored event in a project, newest first. */
  async allEventDetails(slug) {
    const list = await this.events(slug);
    return Promise.all(list.map((e) => this.event(slug, e.event_id, e.issue_id)));
  }

  /** Wait until an event matching `pred(detail)` is stored, return its detail. */
  async waitForEvent(slug, pred, opts = {}) {
    return waitFor(
      async () => (await this.allEventDetails(slug)).find(pred),
      { timeout: 30_000, what: `event in ${slug}`, ...opts },
    );
  }
}

/** Exceptions of an event payload, innermost (thrown) last, as Sentry stores them. */
export function exceptions(detail) {
  return detail.payload?.exception?.values ?? [];
}

/** Message of the event (captureMessage). */
export function messageOf(detail) {
  const le = detail.payload?.logentry;
  return le?.formatted ?? le?.message ?? detail.payload?.message ?? detail.message;
}
