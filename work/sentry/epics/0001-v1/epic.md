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
- 01 server core - done (98284b9)
- 02 client + CLI + TUI (also embeddable as a library) - done (b261f40)
- 03 JS end-to-end tests (26 tests, real SDKs, Chromium, sentry-cli, vite plugin) - done (b261f40)
- 04 deploy: Docker, compose (RustFS + Postgres), Helm (verified on k8s), CI - done (b261f40)
- 05 load test + capacity doc - done ([capacity.md](../../../docs/capacity.md))
- 06 docs + three independent reviews (server security, data model, CLI/TUI);
  all findings fixed or filed as issues 0001-0003 - done

## Progress log
- 2026-11-01: reviews found 12 server, 12 storage and 11 CLI issues; fixed:
  zlib bomb, envelope length overflow, tag/frame/exception caps, CPU work off
  async workers, auth before body read, symbolication lookup caps, failed-map
  caching, bundle indexing per project, ILIKE escaping, two-phase issue list,
  SQL top-N tag facets, cursor tiebreak, snapshot-consistent reads, day-aligned
  retention, staging cleanup, readiness checks the writer, compaction OOM under
  2 GB (single-threaded sorted merges, 64 MB files), CLI escape-code injection,
  broken pipe, ctrl/alt keys in the embedded TUI. Filed: 0001, 0002, 0003.
