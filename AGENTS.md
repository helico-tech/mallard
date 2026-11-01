# obs-ducklake

One observability platform: Sentry-compatible error tracking plus an
OpenTelemetry collector (traces, logs, metrics), stored together in one
DuckLake catalog. Built on two libraries from sibling repos:

- `../sentry-ducklake` (crates `sdl-*`) — Sentry ingest, issues, events.
- `../otel-ducklake` (crates `odl-*`) — OTLP ingest, traces, logs, metrics.

This repo only glues them together and adds what spans both: one server
binary, one CLI, one TUI, error ↔ trace correlation, Docker and Helm.
The libraries are pinned to commits in `libs.lock` (`scripts/pin-libs.sh`,
ADR 0003). Bugs or missing API in the libraries are fixed in their own repo, not
worked around here.

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo run -p obs-server -- --config config/obs-ducklake.example.toml
cargo run -p obs-cli -- --help      # binary `obs`; `obs tui` opens the TUI
docker compose -f deploy/docker-compose.yml up
```

## Map

- `docs/architecture.md` — parts, ports, config, key rules.
- `docs/adr/` — decisions.
- `work/queue.md` — what to pick up next.
- `work/epics/0001-platform/` — the build-out plan.
