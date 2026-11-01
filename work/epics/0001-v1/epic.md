---
status: in-progress
created: 2026-10-31
---
# Epic 0001: v1 - Sentry-compatible error tracking on DuckLake

## Goal
Sentry SDKs work by swapping the DSN. Errors are grouped into issues, stored
in DuckLake on any storage DuckLake supports (S3 default), and explored via
an OpenAPI HTTP API, a CLI and a TUI. JS sourcemaps work with sentry-cli and
bundler plugins. Docker image, Helm chart, docs, measured capacity.

## Scope
- Ingest: envelope + store endpoints, auth, compression, CORS, 429 backpressure.
- Grouping, issue status (resolve/ignore, regression), tags, trace ids.
- Sourcemaps: chunk upload + artifact bundles, debug-id and release lookup.
- Query API (`/v1`), OpenAPI, Scalar docs.
- CLI + TUI, Docker, compose, Helm, load test + capacity doc.
- Library use (embedding by obs-ducklake).

Out of scope for v1: transactions/performance, sessions, replays, attachments,
alerts/notifications, multi-org, user accounts.

## Done when
- Real JS SDKs (node + browser) verified end to end, incl. symbolicated
  frames from sentry-cli uploads.
- Docs complete; image builds; chart lints and installs; load numbers in docs.
- Reviewed by independent agents; findings fixed or filed.

## Stories
- 01 server core - done (commit 98284b9)
- 02 client + CLI + TUI
- 03 JS end-to-end tests
- 04 deploy: Docker, compose, Helm, CI
- 05 load test + capacity doc
- 06 docs + review
