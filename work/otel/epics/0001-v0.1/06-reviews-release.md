---
status: in-progress
created: 2026-11-01
---
# reviews-release

## Goal
Independent agent reviews (correctness, security, ops, docs), fix findings, push to GitHub.

## Acceptance criteria
- Built, tested (cargo test), documented.

## Plan
See epic and docs/architecture.md.

## Progress log
- 2026-11-01: created.
- 2026-11-01: Four independent reviews (ingest, query/security, clients, deployment/docs).
- 2026-11-01: Fixed: pool leak/panics, SQL guard CTE bypass, gRPC back-pressure code, expansion bomb, password leaks, rolling upgrades, TZ cut-offs, metric math, error contract, deployment hardening.
- 2026-11-01: Open: client-fixes merge, push to GitHub.

## Follow-ups
