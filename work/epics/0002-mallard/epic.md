---
status: in-progress
created: 2026-11-01
---
# Epic 0002 — mallard: one codebase, one better product

## Goal
Merge sentry-ducklake, otel-ducklake and obs-ducklake into one repo and one
binary, clean the code up to idiomatic Rust, and use the fact that errors
and telemetry now live in one app to build features neither side had alone.

## Scope
- Monorepo with full history, one workspace, one `mallard` binary, one
  deploy setup, published at github.com/helico-tech/mallard.
- A thorough review of the whole repo; refactor to idiomatic Rust.
- Cross-side features (stories 03–05).

## Done when
- The repo is public, CI is green.
- Review findings are fixed or filed as issues.
- Stories 03–05 are done.

## Stories
1. [01-monorepo](01-monorepo.md)
2. [02-review-refactor](02-review-refactor.md)
3. [03-sentry-transactions-as-traces](03-sentry-transactions-as-traces.md)
4. [04-otel-exceptions-as-issues](04-otel-exceptions-as-issues.md)
5. [05-errors-in-trace-views](05-errors-in-trace-views.md)
6. [06-retire-standalone-binaries](06-retire-standalone-binaries.md)
7. [07-split-server-and-cli](07-split-server-and-cli.md)
8. [08-agent-cli](08-agent-cli.md)
9. [09-readme-screenshots](09-readme-screenshots.md)
10. [10-one-binary](10-one-binary.md)
11. [11-crate-cleanup](11-crate-cleanup.md)
