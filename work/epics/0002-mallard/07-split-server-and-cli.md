---
status: done
created: 2026-11-01
---
# 07 — Separate server and CLI/TUI

## Goal
Server and CLI/TUI are separate crates and binaries: `mallard-server` (DuckDB, ingest, storage) and `mallard` (CLI + TUI, HTTP only, no DuckDB). Supersedes the one-binary part of ADR 0004.

## Acceptance criteria
- `cargo install -p mallard` does not build DuckDB.
- Docker image runs `mallard-server` and also ships `mallard`.
- Helm/compose/CI updated; new ADR records the change.

## Plan

## Progress log
- 2026-11-01: `mallard-server` binary (serve/check/install-extensions) in crates/mallard-server; `mallard` = CLI/TUI only, no duckdb in its dependency tree (debug 41 MB vs 206 MB). Dockerfile builds both, runs mallard-server, ships mallard. ADR 0007. Docker smoke test: readyz ok, `docker exec … mallard status` ok.

## Follow-ups
