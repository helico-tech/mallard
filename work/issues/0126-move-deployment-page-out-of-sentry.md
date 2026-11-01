---
status: done
type: debt
created: 2026-11-01
---
# The deployment page for both sides lives under docs/sentry

## Problem
There is no `docs/deployment.md`. After issue 0016, `docs/sentry/deployment.md` covers
Docker, compose, Helm, storage and backups for the whole server, and
`docs/otel/deployment.md` keeps only telemetry extras, because the scope of 0016 was the
side folders and `NOTES.txt` already links there. A reader looking for "how to deploy"
does not expect it under `sentry/`.

## Notes
Move it to `docs/deployment.md`, link it from the README, and update `NOTES.txt`,
`docs/otel/*` and `docs/sentry/README.md`. `docs/otel/storage-backends.md` could move
with it. Found during issue 0016.

Done. The page is `docs/deployment.md`, and the README, `AGENTS.md`, `docs/architecture.md`,
`docs/otel/*` and `docs/sentry/README.md` link to it. No pointer is left behind, because
nothing external needs one. `docs/otel/storage-backends.md` stays where it is. `NOTES.txt` was
not updated: the chart is removed next (ADR 0026).
