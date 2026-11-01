---
status: done
type: debt
created: 2026-11-01
---
# Per-side docs still describe the standalone binaries

## Problem
`docs/sentry/` and `docs/otel/` (and `crates/sdl-cli/README.md`) were
written for the standalone `sentry-ducklake`/`sdl` and `otel-ducklake`/`odl`
binaries: their flags and `SDL_*`/`ODL_*` env vars, their Docker images and
charts, `cargo run -p sdl-server`. Story 0002/06 removed those binaries and
only put a "retired binary" note on the main pages (READMEs, AGENTS,
deployment, CLI) and fixed the capacity run commands and the config link.

## Notes
Rewrite what is still useful for mallard (storage and multi-replica notes,
data model, compatibility, sourcemaps, operations) against
`mallard-server`, `config/mallard.example.toml` (`[otel.*]`, `[sentry.*]`),
`/sentry` and `/otel` paths and the `mallard` CLI; drop the rest. ADRs in
`docs/*/adr/` stay as history.

Done 2026-11-01. `docs/sentry/*.md`, `docs/otel/*.md` and
`crates/mallard-sentry-cli/README.md` now describe `mallard serve`, the `[otel.*]` and
`[sentry.*]` config, the `/sentry` and `/otel` paths, the current compose file and
chart. `docs/sentry/deployment.md` is the deployment page for both sides;
`docs/otel/deployment.md` keeps telemetry extras. Both `AGENTS.md` are pointers.
`capacity.md` numbers are unchanged, with a note that they are from the old binaries;
the sentry load command gained `--duration 1h`, because the default of 30 s cut the run
short.

Checked by running: `--help` of the commands, the compose stack (both catalog modes),
`docker run --read-only`, `mallard serve` with the CLI and API examples, `sentry-cli`
3.8.2 upload, e2e `unhappy`, `vite-plugin` and `browser-sourcemaps`, `helm template` and
`lint`, `mallard check` on every TOML sample, small runs of the capacity commands.

Not checked: real S3, GCS, Azure and R2 (the TOML samples only parse); `helm install`
and kind (kind is not installed here, CI covers it); several replicas; the opening of
the lake with the DuckDB CLI (the local CLI is 1.5.6 and refuses the catalog, which the
doc now says); release builds; the capacity numbers.

Found along the way: issues 0122 to 0126.
