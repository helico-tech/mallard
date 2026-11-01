// Run the Node fixture app (fixtures/node-app.mjs) in a child process.

import { spawn } from "node:child_process";
import path from "node:path";
import { E2E_DIR } from "./server.mjs";

/**
 * Run one scenario. Resolves with `{ code, stdout, stderr, ids }` where
 * `ids` are the event ids the app printed (absent for crashing scenarios).
 */
export function runNodeApp(dsn, scenario, tag = "run", { timeout = 60_000 } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(E2E_DIR, "fixtures", "node-app.mjs"), scenario, tag], {
      env: { ...process.env, DSN: dsn },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`node-app ${scenario} timed out\nstdout: ${stdout}\nstderr: ${stderr}`));
    }, timeout);
    child.on("exit", (code) => {
      clearTimeout(timer);
      let ids;
      const line = stdout.trim().split("\n").filter(Boolean).pop();
      try {
        ids = line ? JSON.parse(line).ids : undefined;
      } catch {
        ids = undefined;
      }
      resolve({ code, stdout, stderr, ids });
    });
  });
}
