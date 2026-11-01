---
status: todo
type: idea
created: 2026-11-01
---
# Store releases made with `sentry-cli releases new/finalize/deploys`

## Problem
The `/api/0/.../releases` endpoints are stubs: they answer as if the release
was stored, and nothing is. `mallard releases list` derives releases from the
`release` of error events, so a release without errors (or a deploy) is not
seen, and `sentry-cli releases list` shows nothing.

## Notes
Left as stubs in [issue 0014](0014-agent-cli-p1-leftovers.md) because storing
them is not small:

- a new append-only table and writer command, on the writer thread;
- a schema change with no migrations yet (`SCHEMA_VERSION`, `db/schema.rs`);
- deciding how stored releases merge with the derived list (a release with
  no events; `dateReleased` versus first seen; deploys per environment; which
  projects a release belongs to, the org-level endpoint has none).

Nothing needs it today: the deploy-tracking jobs agents have (new issues since a
release, `issues list --new`, `--first-release`) work from events. The stubs
are documented in `docs/sentry/compatibility.md`, `docs/agents.md` and
`docs/cli.md`. Pick this up when someone needs deploy markers.
