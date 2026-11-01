---
status: done
created: 2026-11-01
---
# 01 — Monorepo and one binary

## Goal
All three repos in one, with history; one workspace; `mallard` = server + CLI + TUI; one Docker/compose/Helm; pushed.

## Acceptance criteria
- `git log` shows the history of all three.
- `cargo test --workspace`, clippy -D warnings and fmt pass.
- Docker image builds from the repo alone; compose flow works.
- Public on GitHub with a license; CI green.

## Plan

## Progress log
- 2026-11-01: Merged with full history of all three repos, pushed public to github.com/helico-tech/mallard (MIT OR Apache-2.0), CI green (Rust, Helm, Docker, Sentry JS e2e).

## Follow-ups
