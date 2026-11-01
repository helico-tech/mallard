---
status: todo
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
