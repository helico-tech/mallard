// Starts a throwaway server with demo data and `mallard web` in both modes.
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "../../../..");
const bin = (name: string) => process.env[`${name.toUpperCase().replace("-", "_")}_BIN`] ?? path.join(root, "target/debug", name);
const MALLARD = bin("mallard");
export const SERVER = "http://127.0.0.1:38080";
export const OTLP = "http://127.0.0.1:34318";
export const TOKEN = "e2e-token";

async function waitFor(url: string) {
  for (let i = 0; i < 120; i++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // Not up yet.
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`${url} did not come up`);
}

export default async function setup() {
  const dir = mkdtempSync(path.join(tmpdir(), "mallard-web-e2e-"));
  const procs: ChildProcess[] = [];
  const run = (cmd: string, env: Record<string, string>, args: string[] = []) => {
    const p = spawn(cmd, args, { cwd: dir, env: { ...process.env, ...env }, stdio: "ignore" });
    procs.push(p);
  };
  run(MALLARD, {
    MALLARD_SERVER__API_TOKENS: `["${TOKEN}"]`,
    MALLARD_SERVER__PUBLIC_URL: SERVER,
    MALLARD_SERVER__HTTP_ADDR: "127.0.0.1:38080",
    MALLARD_SERVER__OTLP_HTTP_ADDR: "127.0.0.1:34318",
    MALLARD_SERVER__OTLP_GRPC_ADDR: "127.0.0.1:34317",
    MALLARD_OTEL__INGEST__FLUSH_INTERVAL: "1s",
  }, ["serve"]);
  await waitFor(`${SERVER}/readyz`);
  run(MALLARD, { MALLARD_URL: SERVER, MALLARD_TOKEN: TOKEN }, ["web", "--addr", "127.0.0.1:38088"]);
  run(MALLARD, { MALLARD_URL: SERVER, MALLARD_TOKEN: "" }, ["web", "--addr", "127.0.0.1:38089"]);
  await waitFor("http://127.0.0.1:38088/healthz");
  await waitFor("http://127.0.0.1:38089/healthz");

  const seed = spawnSync("python3", [path.join(root, "scripts/demo-seed.py"), "--url", SERVER, "--token", TOKEN, "--otlp", OTLP], { encoding: "utf8" });
  if (seed.status !== 0) throw new Error(`seeding failed: ${seed.stderr}`);
  await new Promise((r) => setTimeout(r, 3000));

  return () => {
    for (const p of procs) p.kill();
    rmSync(dir, { recursive: true, force: true });
  };
}
