import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// `npm run dev` proxies the API and the session routes to a running
// `mallard web` (default port), so the dev server never holds a token.
const backend = process.env.MALLARD_WEB_BACKEND ?? "http://127.0.0.1:8088";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
  build: { outDir: "dist", emptyOutDir: true, assetsInlineLimit: 0, chunkSizeWarningLimit: 1500 },
  server: {
    proxy: {
      "/sentry": { target: backend, changeOrigin: false },
      "/otel": { target: backend, changeOrigin: false },
      "/web": { target: backend, changeOrigin: false },
    },
  },
  test: { environment: "node", include: ["src/**/*.test.ts"] },
});
