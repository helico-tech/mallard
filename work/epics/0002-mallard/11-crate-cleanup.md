---
status: todo
created: 2026-11-01
---
# 11 — Crate cleanup: one naming scheme, no standalone leftovers

## Goal
The crates still carry the names and set-up of the two old projects. A new
reader should see one product.

## What is wrong (checked 2026-11-01)
- `sdl-*` and `odl-*` mean nothing to a new reader, next to `mallard-*`.
- `sdl-types` and `odl-api` do the same job under different names.
- `sdl-server/src/standalone.rs` (294 lines of clap flags and `SDL_*` env
  vars) survives only for tests; its binary is gone.
- OpenAPI titles still say `sentry-ducklake API` and `otel-ducklake query
  API`; about 25 source files still use the old names or env vars.

## Acceptance criteria
- Crates renamed: `sdl-*` to `mallard-sentry-*`, `odl-*` to `mallard-otel-*`,
  `sdl-types` to `mallard-sentry-api`.
- `standalone.rs` replaced by a small test helper; `SDL_*` and `ODL_*` env
  vars are gone from code and docs, unless a test needs its own.
- Old product names are gone outside `work/` and history notes. Metric names
  (`sdl_*`, `odl_*`) are renamed too, since nothing outside this repo uses
  them yet.
- Tests, e2e, Docker, Helm and OpenAPI snapshots pass.

## Plan
Run after story 10, as one mechanical change per step, and before the docs
rewrite in issue 0016.

## Progress log

## Follow-ups
