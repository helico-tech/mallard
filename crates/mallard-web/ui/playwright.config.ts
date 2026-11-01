import { defineConfig } from "@playwright/test";

// Real `mallard serve` plus `mallard web`, on private ports.
// Build first: `npm run build`, then `cargo build -p mallard`.
export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/setup.ts",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  reporter: [["list"]],
  use: { baseURL: "http://127.0.0.1:38088", trace: "retain-on-failure" },
});
