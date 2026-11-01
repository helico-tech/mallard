---
status: done
created: 2026-11-01
---
# 06 — Retire the standalone binaries

## Goal
The e2e suite and load generators run against `mallard`, so `sentry-ducklake`, `otel-ducklake`, `sdl` and `odl` binaries can go.

## Acceptance criteria
- e2e/sentry runs against `mallard-server` (paths under /sentry).
- Load generators target mallard.
- Only the `mallard-server` and `mallard` binaries are built (plus the two
  load generators, which are dev tools); the library crates stay.

## Plan
1. e2e harness (`e2e/sentry/lib/server.mjs`): start `mallard-server` with a
   temp config file (one free port for `http_addr`, OTLP listeners on
   `127.0.0.1:0`, temp catalog/lake/artifacts). `server.url` becomes
   `<base>/sentry`, so the tests keep their relative paths; `server.base`
   for `/readyz` and `/otel/...`. Binary: `$MALLARD_SERVER_BIN`, else
   `target/debug/mallard-server` (built if missing). CI builds
   `-p mallard-server`.
2. Remove the `[[bin]]`s and `main.rs` of `sdl-server` (`sentry-ducklake`),
   `odl-server` (`otel-ducklake`), `sdl-cli` (`sdl`), `odl-cli` (`odl`).
   - `openapi` subcommands: the snapshot tests regenerate the file with
     `UPDATE_OPENAPI=1 cargo test -p sdl-server|odl-server --test openapi`
     (no product surface; the served `/openapi.json` stays).
   - `sdl-cli/tests/cli.rs` ran the `sdl` binary: port to
     `crates/mallard/tests/sentry_cli.rs` against the `mallard` binary
     (paths under `/sentry`); the odl `-o json` test goes with its binary.
   - `config/otel-ducklake.example.toml` only served `otel-ducklake config`
     and its parse test: remove it and `Config::example_toml()`;
     `config/mallard.example.toml` covers `[otel.*]`.
   - The library APIs used by tests (`sdl_server::Standalone`,
     `odl_server::Server::new`, `run_maintenance`) stay.
3. Load generators: `sdl-loadgen --url` is the mallard base URL
   (`MALLARD_URL`/`MALLARD_TOKEN`, default `http://localhost:8080`, Sentry
   under `/sentry`); `odl-loadgen` already sends OTLP/HTTP to :4318. Bench
   scripts in `bench/otel/` run the mallard image / `mallard-server`.
4. Docs: AGENTS.md, architecture, e2e README, per-side READMEs/AGENTS
   (a note that the standalone binaries are gone), capacity run commands.
   The rest of `docs/sentry/`, `docs/otel/` still describes the standalone
   setup: file an issue rather than rewrite here.

## Progress log
- 2026-11-01: e2e harness starts `mallard-server` from a temp `mallard.toml`
  (`server.url` = `<base>/sentry`). The Rust sentry-cli refuses a `--url`
  with a path ("not on URL root"), so the sourcemap tests failed:
  `mallard-server` now also serves sentry-cli's `/api/0/...` on the root
  (`nest_service` + prefix put back; a wildcard route added a path param
  the Sentry handlers choked on, 500). Test: `tests/sentry_cli_root.rs`.
  Fixes the other branch's issue "sentry-cli needs root URL".
- 2026-11-01: removed the `sentry-ducklake`, `otel-ducklake`, `sdl`, `odl`
  bins. `openapi`: `UPDATE_OPENAPI=1 cargo test -p … --test openapi`.
  `sdl-cli/tests/cli.rs` → `crates/mallard/tests/sentry_cli.rs` (`mallard`
  binary, `/sentry` paths; `tui` → `errors tui`, since `mallard tui` checks
  the server first). Removed `config/otel-ducklake.example.toml` and
  `Config::example_toml()`. `sdl-loadgen --url` = mallard base
  (`MALLARD_URL`/`MALLARD_TOKEN`); bench scripts run the mallard image /
  `mallard-server`. Dockerfile and Helm already built/ran only
  `mallard-server` + `mallard`: unchanged.
- 2026-11-01: lost on the way: `otel-ducklake maintenance [--full]`
  (one-shot) → issue 0015. Per-side docs still describe the standalone
  setup; notes on the main pages, rewrite → issue 0016.
- 2026-11-01: checks: fmt, clippy `-D warnings`, `cargo test --workspace`
  green; `npm test` in `e2e/sentry` (all 6 files, Chromium): 27 pass,
  1 todo (issue 0017).

## Follow-ups
- [issue 0015](../../issues/0015-one-shot-otel-maintenance.md) one-shot maintenance command
- [issue 0016](../../issues/0016-per-side-docs-for-mallard.md) per-side docs for mallard
