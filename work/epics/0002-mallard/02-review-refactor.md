---
status: in-progress
created: 2026-11-01
---
# 02 — Thorough review and idiomatic Rust

## Goal
Review the whole repo (all crates) and refactor towards idiomatic Rust: error types, module layout, naming, ownership, async/blocking boundaries, duplication between the two sides, tests.

## Acceptance criteria
- Findings listed in this file, each fixed or filed as an issue.
- No public behaviour change without a test.
- Shared code between sdl-* and odl-* extracted where it removes real duplication.

## Plan
Review done 2026-11-01 by five parallel read-only reviewers; findings in
[review/](review/) (≈180: 18 bug, ~15 high). Fix order:
1. Bugs + high + medium per area, in parallel worktrees (one branch each):
   `fix/review-sdl-server`, `fix/review-odl-server`, `fix/review-sdl-cli`,
   `fix/review-odl-cli`. `mallard-*` bugs (cross-cutting B1–B4, open OTel
   API) done on main. Every bug gets a regression test.
2. Cross-side unification (cross-cutting job 2): shared crates
   `mallard-common` (durations/bytes/windows, one config loader, bearer
   auth, JSON errors, signals/logging/metrics), `mallard-duck` (duckdb pin,
   DuckLake setup, quoting, redaction), `mallard-term` (CLI output, escape
   sanitizing, error hints, TUI theme/keys). One user-facing style for
   `mallard errors` and `mallard telemetry`.
3. Remaining low findings; final clippy pedantic pass; docs.

## Progress log

## Follow-ups
