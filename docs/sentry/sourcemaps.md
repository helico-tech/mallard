# JavaScript sourcemaps

Upload sourcemaps with the normal Sentry tools; point them at this server.
Errors then show original files, lines, function names and source code, and
issues stay the same across builds.

You need: the server URL, its API token (`SDL_API_TOKEN`) and a project slug.
The org can be any value (`default` is fine).

## Vite / webpack / Rollup / esbuild plugin

```ts
// vite.config.ts
import { defineConfig } from "vite";
import { sentryVitePlugin } from "@sentry/vite-plugin";

export default defineConfig({
  build: { sourcemap: "hidden" },
  plugins: [
    sentryVitePlugin({
      url: "https://errors.example.com",
      authToken: process.env.SDL_TOKEN,
      org: "default",
      project: "web",
      release: { name: process.env.RELEASE },        // optional
      sourcemaps: { filesToDeleteAfterUpload: ["./dist/**/*.map"] },
    }),
  ],
});
```

Use an absolute or `./dist`-anchored glob in `filesToDeleteAfterUpload`: the
plugin resolves globs against the current directory, and `**/*.map` also
matches files in `node_modules`.

## sentry-cli

```sh
npx sentry-cli sourcemaps inject ./dist          # adds debug ids to .js and .map
npx sentry-cli --url https://errors.example.com --auth-token "$SDL_TOKEN" \
  sourcemaps upload --org default --project web ./dist
```

Add `--release <name>` to also link the bundle to a release. Works with
sentry-cli 2.x and 3.x.

Release-only (no debug ids, e.g. an old build pipeline): skip `inject`, upload
with `--release <name>` (and `--url-prefix '~/assets'` if the files are served
from a sub-path), and set the same `release` in `Sentry.init`.

## How matching works

1. The browser SDK sends `debug_meta.images` with the debug id of each loaded
   script. The server looks up the sourcemap with that debug id in the
   event's project.
2. Otherwise it looks for a minified file with the frame's URL (`~/path`)
   under the event's `release` (and `dist`) and follows its `sourceMappingURL`.
3. Frames are rewritten; files under `node_modules` become `in_app: false`.
   The original frames stay in `raw_stacktrace`; `frame.data.resolved_with` says
   `debug-id` or `release`.

Upload maps **before** the release goes live: events are symbolicated when
they arrive, and stored events are not reprocessed.

## Troubleshooting

- `sdl sourcemaps list -p web` shows uploaded bundles and their debug ids.
- No symbolication: check that the debug id in the event
  (`sdl events show <id> --json | jq .payload.debug_meta`) is in that list,
  and that the bundle was uploaded to the same project.
- `401` from the CLI: wrong `--auth-token`.
- `501`: the server has no storage for artifacts (in mallard: `[storage]
  artifacts_path` is empty).
