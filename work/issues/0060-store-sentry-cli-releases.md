---
status: done
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

## Design (2026-11-03)
The owner wants this for 0.2.0. Decided: [ADR 0035](../../docs/adr/0035-stored-releases-and-deploys.md).

What it changes in the notes above: a new table needs no migration tool, since `db::migrate` creates tables at each start. And
`sentry-cli releases new` does name its projects, in the body; `finalize` and `deploys` do not, so those rows have no project.

## Plan
1. Tables, `WriteCmd`s and reads in `db/`, with tests.
2. The stubs in `api.rs` store for real; new `GET`s for the release and deploy lists. The merged list.
3. `Release` API fields, regenerated OpenAPI types, `mallard releases list`.
4. The e2e suite with real `sentry-cli` 3.x and 2.x, and the Vite plugin.
5. Docs. Measure the list on many events.

## Progress log
2026-11-03. Built as in the ADR. Every step of the plan is done.

**Upgrade, checked with real binaries:** a lake written by `v0.1.0`, opened by this build, back to `v0.1.0` (still reads and writes, ignores the new
tables), and this build again: nothing lost, no error lines. It was on a DuckDB-file catalog only, not Postgres or SQLite.

**Real clients:** `tests/releases.test.mjs` with `sentry-cli` 3.8.2 and 2.x.

**Speed:** about 20 ms for the list, on a lake filled at full speed for 150 s (389 MB). The number of events was not recorded, and a full 14 days was not measured.

**On the way:** building `v0.1.0` in a worktree with the same `CARGO_TARGET_DIR` left old code in the cache, and later builds in the main checkout said
`Finished` without rebuilding. An upgrade check that proves which binary runs caught it. `cargo clean -p` on the workspace's crates fixed it. Do not share a target
directory between a worktree and the main checkout.

Follow-up: [issue 0195](0195-deploy-markers-on-charts.md).
