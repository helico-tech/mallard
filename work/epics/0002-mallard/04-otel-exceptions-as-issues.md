---
status: todo
created: 2026-11-01
---
# 04 — OTel exceptions become Sentry issues

## Goal
Exceptions recorded by OpenTelemetry SDKs (span events named `exception`, and logs with `exception.*` attributes) are grouped into issues on the errors side, so OTel-only apps get issue tracking.

## Acceptance criteria
- An OTel span with an exception event creates/updates an issue with type, message and stack trace.
- The issue links back to the trace.
- Opt-out switch in config; no duplicates when an app sends both Sentry and OTel.

## Plan

## Progress log

## Follow-ups
