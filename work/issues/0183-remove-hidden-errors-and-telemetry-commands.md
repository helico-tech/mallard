---
status: done
type: debt
created: 2026-11-02
---
# Remove the hidden `errors` and `telemetry` commands

## Problem
`mallard errors …` (`sentry`) and `mallard telemetry …` (`otel`) were a second path to every command, kept
by ADR 0008. Nothing is released, so the owner decided to remove them. A duplicate path goes, not a feature.

## Notes
Done, see ADR 0032. `mallard errors` is exit 2.

What the aliases offered, and where it is now:

| old | now |
|---|---|
| `errors issues`, `events`, `projects`, `releases`, `sourcemaps`, `send-test` | the same name |
| `errors sql` | `sql --errors` |
| `errors info` | `status --json`, `sentry.info` (the text has version, DuckDB and queue, not catalog and storage) |
| `errors stats` | **only under the alias**: new `stats` (`series errors` has no totals) |
| `errors maintenance` | **only under the alias**: new `flush` (`maintenance` needs the config and works without a server) |
| `errors tui` | `tui`, F1 |
| `telemetry health` | **only under the alias**: new `health` (`status` needs a token) |
| `telemetry status` | `status --json`, `otel.info` (the text is a summary) |
| `telemetry services`, `ops`, `logs`, `sql` | the same name |
| `telemetry traces`, `metrics`, `metric`, `attrs` | `traces list`, `metrics list`, `metrics show`, `schema attrs` |
| `telemetry trace [-A]` | `trace`; attributes and events with `--json` |
| `telemetry schema` | `schema` (both sides) |
| `telemetry tui [-s]` | `tui`, F2; `t` changes the window, there is no start flag |

Moving the tests found two things only the alias did, both fixed in `mallard`:
- `mallard tui` panicked without a terminal; `errors tui` gave a message. Now it gives that message.
- The text of `mallard trace` left out spans in a parent loop; `telemetry trace` showed them. Now it shows them.

Not done: `bench/otel/capacity/steady-state.sh` now uses `status --json | jq`, not run. See issues 0184 and 0185.
