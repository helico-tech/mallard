# 0008 — One command tree and an output contract for agents

## Status
accepted (supersedes the command naming of 0002 and 0006: `mallard errors …`
and `mallard telemetry …` are no longer the main paths)

## Context
The main users of the `mallard` CLI are AI coding agents and CI scripts;
people at a terminal come second but must keep a good experience. The
research in [docs/specs/agent-cli.md](../specs/agent-cli.md) found that the
commands covered most jobs, but the contract did not:

- every error was exit code 1 with human text on stderr, and `status`
  exited 0 when the server was down;
- JSON shapes differed per command (`{issues:[…]}`, `{events:[…]}`, bare
  arrays) and were pretty-printed (about 2x the tokens);
- no way to trim fields; raw Sentry payloads and per-record resource
  attributes made JSON large;
- box tables and sparklines when piped;
- the Sentry side had no `--from/--to`; no "new in this release", no
  idempotent project setup, no OTLP env output, no "what is this id";
- paths were long and split by the two internal sides
  (`mallard errors issues list`, `mallard telemetry traces`).

ADR 0002 made the libraries' command sets show up in the host "for free"
under `errors`/`telemetry`. ADR 0006 unified their flags and output style.

## Decision

**Command tree.** `mallard` owns one flat tree of nouns; the commands
underneath stay in `sdl-cli` and `odl-cli`, which expose them as `*Args` /
`*Cmd` types the host mounts one by one:

```
mallard status | setup <name> | find <id>
        issues   list | show <id> | resolve|ignore|unresolve <id>… [--reason]
        events   list | show <event-id>
        projects list | show <p> | create <name> [--if-not-exists]
        traces   list | show <trace-id>        trace <trace-id> (= traces show)
        logs     [--around <time> --window 1m] [-f]
        services | ops | metrics list|show <name> | sql "<select>"
        sourcemaps list | send-test | tui | help [agents|<command>]
```

`mallard errors …` (also `sentry`) and `mallard telemetry …` (also `otel`)
stay as hidden aliases: the same library commands with the same contract.
Commands that span both sides (`status`, `setup`, `find`, `trace`) live in
`crates/mallard`. Global flags: `--url`, `--token`, `-p/--project`,
`--format`, `--json`, `--fields`, `--raw`, `--color`.

**Output contract** (in `mallard-term`, shared by both libraries):

- stdout is data only; errors, notes and hints go to stderr; nothing
  prompts or pages (only `tui` needs a terminal).
- `--format text|json|ndjson` (env `MALLARD_FORMAT`; `--json` = `--format
  json`). JSON is pretty on a terminal and compact when piped. Text is
  plain when piped: no box drawing, no sparklines, no colour, one row per
  line.
- Lists are one envelope: `{"items", "count", "limit", "truncated",
  "next_cursor", "window"}`; `window` is the resolved absolute UTC window.
  `ndjson` prints one item per line without the envelope (truncation note
  on stderr). Single objects are printed as themselves.
- `--fields a,b` keeps those top-level keys, in that order; an unknown
  field is a usage error listing the valid ones.
- Concise by default; `--raw` adds raw payloads (Sentry `payload`, log and
  span resource attributes).
- Windows: `-s/--since`, `--from`, `--to` everywhere a window applies;
  `--from`/`--to` take RFC 3339 or a duration ago. The CLI resolves the
  window once and sends absolute `from`/`to` to both sides.

**Errors and exit codes.** Each failure has a kind (`mallard_term::error::
Kind`) with its own exit code; HTTP statuses map to kinds in one place:

| code | kind | from |
|---:|---|---|
| 0 | success (also empty results) | |
| 1 | internal | 500, unreadable answer |
| 2 | usage | clap, bad id/window/field, HTTP 400 |
| 3 | auth | 401, 403 |
| 4 | not found | 404, `find`/`trace` without a match |
| 5 | conflict | 409 |
| 6 | unavailable (retryable) | connect error, timeout, 429, 502–504, `status` with a side down |
| 7 | disabled | 403 `disabled` (SQL), 501 |
| 8 | condition not met | reserved for `--wait` / `issues check` |

With `--format json|ndjson` an error is one JSON line on stderr:
`{"error":{"code","message","status","retryable","hint"}}`. In text mode it
stays one line, `error: message (hint)`. A closed pipe is still success.

**Server side.** Each side adds what the CLI needs to its own API
(additive, no new `mallard-server` API): Sentry `issues?new=&first_release=`,
event list windows, `PATCH issue` with `reason` and `changed`,
`GET /v1/events/{id}` across projects, `public_url`/retention/max window in
`/v1/info`; OTel `capabilities` in `/api/v1/stats`.

**Agent guide.** `docs/agents.md` is built into the binary
(`mallard help agents`); a test checks they are the same. Every command's
`--help` has examples.

## Consequences
- Agents get stable shapes and can branch on exit codes and error codes;
  retries are safe for exit 6 only.
- `--json` output changed shape (envelope; concise events; `issues
  resolve` answers `{"results":[{id,status,changed}]}`) — a one-time break
  before 1.0. Errors that used to exit 1 now exit with their kind's code.
- New commands are added to the flat tree in `crates/mallard` (one line
  each); the "for free" mounting of ADR 0002 remains only for the hidden
  aliases and the standalone `sdl`/`odl` binaries.
- JSON key order follows the structs, except where `--fields` sets it.
- Not done yet (spec P1): `schema`, `--wait`, `logs -f --for`, `ops
  --compare`, `series`, tag filters, span ids in `find`, cursors as flags.
