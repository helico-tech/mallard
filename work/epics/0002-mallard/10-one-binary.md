---
status: todo
created: 2026-11-01
---
# 10 — One `mallard` binary

## Goal
One thing to install: `mallard serve` runs the server, `mallard tui` and the
CLI talk to it over HTTP. The user chose this on 2026-11-01 (option A) over
two binaries (ADR 0007) and over `mallard serve` starting a second binary.

## Acceptance criteria
- `crates/mallard` builds the only product binary. Feature `server` is on by
  default and adds `serve`, `check [--connect]`, `install-extensions` and
  `maintenance [--full]`, from the `mallard-server` library crate.
- `--no-default-features` builds the same CLI and TUI with no DuckDB in
  `cargo tree`; CI checks this.
- `mallard-server` stays a library crate and has no binary.
- Docker, compose, Helm, CI, e2e, scripts and docs use `mallard serve`.
- A new ADR supersedes the binary split in ADR 0007. The crate split stays.

## Plan
1. Wait until the agents now running have merged; they change the same files.
2. Move `mallard-server`'s `main.rs` commands behind the feature.
3. Move the tests that run the binary from `mallard-server/tests` to
   `crates/mallard/tests`, to avoid a dependency cycle.
4. Update image, chart, CI, e2e, scripts, docs, ADR.

## Progress log

## Follow-ups
