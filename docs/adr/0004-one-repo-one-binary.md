# 0004 — One repository, one binary: mallard

## Status
accepted (supersedes 0003); the one-binary part is superseded by 0007

## Context
The platform was three repos: sentry-ducklake, otel-ducklake and
obs-ducklake (the glue). Every cross-cutting change needed a request to
another repo, a new commit there and a pin bump here (ADR 0003). Features
that span both sides (Sentry transactions as traces, OTel exceptions as
issues) would need even more of that.

## Decision
- One repo, `helico-tech/mallard`, with the full history of all three
  (each moved into the new layout in one commit, then merged).
- One Cargo workspace. The Sentry crates (`sdl-*`) and OTel crates
  (`odl-*`) keep their names and boundaries; `mallard-*` bundles them.
- One product binary, `mallard` (server + CLI + TUI). The standalone
  `sentry-ducklake` / `otel-ducklake` binaries stay for the e2e suite and
  load tests until those run against `mallard`.
- One Docker image, compose stack and Helm chart (`deploy/`); the per-side
  ones are removed. Side-specific docs live in `docs/sentry/`, `docs/otel/`.
- One work queue (`work/queue.md`).

## Consequences
- Cross-cutting changes are one commit and one review.
- The crate boundaries from ADRs 0001/0002 still hold (shared connection,
  embeddable CLI/TUI), so each side can be tested alone.
- `libs.lock`, `scripts/pin-libs.sh` and the `obs-pin` worktrees are gone.
