// Run the Node fixture apps (fixtures/*.mjs) in a child process.

import { spawn } from "node:child_process";
import path from "node:path";
import { E2E_DIR } from "./server.mjs";

/**
 * Run `fixtures/<file>` with `args` and `DSN=dsn`. Resolves with
 * `{ code, stdout, stderr, last }` where `last` is the last stdout line
 * parsed as JSON (absent when it is not JSON, e.g. after a crash).
 */
export function runFixture(file, dsn, args = [], { timeout = 60_000 } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(E2E_DIR, "fixtures", file), ...args], {
      env: { ...process.env, DSN: dsn },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`${file} ${args.join(" ")} timed out\nstdout: ${stdout}\nstderr: ${stderr}`));
    }, timeout);
    child.on("exit", (code) => {
      clearTimeout(timer);
      let last;
      const line = stdout.trim().split("\n").filter(Boolean).pop();
      try {
        last = line ? JSON.parse(line) : undefined;
      } catch {
        last = undefined;
      }
      resolve({ code, stdout, stderr, last });
    });
  });
}

/**
 * Run one scenario of fixtures/node-app.mjs. Resolves with
 * `{ code, stdout, stderr, ids }` where `ids` are the event ids the app
 * printed (absent for crashing scenarios).
 */
export async function runNodeApp(dsn, scenario, tag = "run", opts = {}) {
  const { last, ...r } = await runFixture("node-app.mjs", dsn, [scenario, tag], opts);
  return { ...r, ids: last?.ids };
}
