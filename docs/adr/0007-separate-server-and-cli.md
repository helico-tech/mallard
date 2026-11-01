# 0007 — Separate server and CLI/TUI binaries

## Status
accepted (supersedes the one-binary part of 0004)

## Context
ADR 0004 bundled the server and the CLI/TUI into one `mallard` binary. The
CLI is used far more widely than the server: on laptops, in CI, and by AI
coding agents that need a quick install. Linking the server means linking
DuckDB (compiled from C++), the storage stack and every ingest dependency
into a tool that only speaks HTTP. The debug CLI was ~200 MB and took the
full DuckDB build to install.

## Decision
- `crates/mallard-server` builds the `mallard-server` binary:
  `mallard-server [-c file] [serve|check [--connect]|install-extensions]`.
- `crates/mallard` builds the `mallard` binary: CLI and TUI only. It depends
  on the client and terminal crates, never on `mallard-server`,
  `mallard-duck` or `duckdb` (`cargo tree -p mallard` has no duckdb).
- The Docker image runs `mallard-server` and also ships `mallard` for
  `docker exec … mallard status`.

## Consequences
- `cargo install --path crates/mallard` is quick and the CLI is ~40 MB
  (debug) instead of ~200 MB.
- Two names to learn: `mallard-server` runs it, `mallard` uses it.
- The ignored screenshot test and the e2e test still run the server
  in-process or via `mallard-server`.
