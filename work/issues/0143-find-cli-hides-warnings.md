---
status: todo
type: debt
created: 2026-11-01
---
# `mallard find` does not show that a source failed

## Problem
`GET /api/v1/find` returns `warnings` when a source failed but something else was
found: the list may be incomplete, for example the event without its trace.
`mallard find` drops them, as it dropped such failures before it used the endpoint.
Story 0004/01 asked for the same output, so it stays. An agent cannot tell.

## Notes
Printing `warning: …` on stderr does not touch stdout or the exit code, and
`mallard apps` and `mallard trace` already do it.
