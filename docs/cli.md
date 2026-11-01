# mallard CLI and TUI

The `mallard` CLI/TUI (ADR 0007: separate from `mallard-server`), two
sides that look and behave alike (ADR 0006). The shared
pieces live in `crates/mallard-term`.

## Commands

One flat tree (ADR 0008). For AI agents: [agents.md](agents.md), also
`mallard help agents`. Every command's `--help` has examples.

| command | what |
|---|---|
| `mallard status` | health of both sides and what the server allows (SQL, windows, retention); exit 6 when a side is down |
| `mallard setup <name> [--platform p]` | get or create the project; print `SENTRY_DSN` and the `OTEL_EXPORTER_OTLP_*` env vars |
| `mallard find <id>` | a trace or event id (32 hex) or an issue id (16 hex): what it is and the command to see more |
| `mallard issues list\|show\|resolve\|ignore\|unresolve` | error issues; `list --new`, `--first-release`, `--tag k=v` (repeat), `--cursor`; status changes take `--reason` |
| `mallard events list\|show` | error events; `list --tag k=v`, `--release`, `--env`, `-q`, `--cursor`; `show --json` is concise, `--raw` adds the Sentry payload |
| `mallard releases list` | releases seen in error events: first/last seen, events, issues, new issues (derived from events; releases made with `sentry-cli releases new` are not stored) |
| `mallard projects list\|show\|create [--if-not-exists]` | Sentry projects and their DSNs |
| `mallard traces list\|show`, `mallard trace <id>` | search traces (with a `sentry` error count per trace); one trace with its spans, Sentry errors (`⚠ n` on the span) and logs |
| `mallard logs` | search (`-S`, `-l`, `-q`, `-t`, `-a`), follow (`-f`), or `--around <time> --window 1m` |
| `mallard services`, `mallard ops` | request rate, error rate, latency per service / operation |
| `mallard metrics list\|show <name>` | metric names; one metric over time (`-g p95 -b http.route`) |
| `mallard sql "<select>" [--errors]` | read-only SQL over the telemetry tables, or with `--errors` over `events`, `issues`, `issue_activity` (exit 7 when off; ADR 0010) |
| `mallard sourcemaps list`, `mallard send-test` | uploaded bundles; send a test error to a DSN |
| `mallard tui [-p project]` | both TUIs as tabs: F1 errors, F2 telemetry |

The old paths still work, hidden: `mallard errors …` (= the `sdl` command
set: `info`, `stats`, `maintenance`, …) and `mallard telemetry …` (= `odl`:
`health`, `status`, `metric`, waterfall `trace`, …). Side details:
[crates/sdl-cli/README.md](../crates/sdl-cli/README.md), [otel/cli.md](otel/cli.md).

## Shared flags

Global (any position):

| flag | env | meaning |
|---|---|---|
| `--url` | `MALLARD_URL` | server base URL |
| `--token` | `MALLARD_TOKEN` | API token |
| `-p, --project` | `MALLARD_PROJECT` | Sentry project id or slug; optional with one project |
| `--format text\|json\|ndjson` | `MALLARD_FORMAT` | output format; `--json` = `--format json` |
| `--fields a,b` | | keep only these top-level fields (unknown field: exit 2) |
| `--raw` | | include raw payloads (Sentry `payload`, resource attributes) |
| `--color auto\|always\|never` | `NO_COLOR` | `auto`: colour only on a terminal, without `NO_COLOR` and with `TERM` not `dumb` |

Per command, the same letter for the same thing on both sides:

| flag | meaning |
|---|---|
| `-s, --since 1h` | time window ending at `--to` or now |
| `--from`, `--to` | RFC 3339 or a duration ago (`--from 2h --to 1h`) |
| `-n, --limit N` | max rows (`-l` still works for `traces list`) |
| `-q, --query text` | text search |
| `-t, --trace id` | only this trace (`events list`, `logs`) |
| `--cursor c` | the next page: `next_cursor` of the previous answer (`issues list`, `events list`) |
| `-S, --service name` | only this service (telemetry) |

Default windows differ where the data does: issues 14d, events all (30d
with `--trace`), traces/logs/metrics 1h, `trace` and `find` 7d.

## Output

- stdout is data only; errors and notes go to stderr. Nothing prompts.
- Text on a terminal: tables in one style (rounded, bold headers),
  sparklines, colour. **Piped**: plain text, one row per line, columns two
  spaces apart, no boxes, sparklines or colour.
- JSON: pretty on a terminal, compact when piped. Lists are an envelope
  `{"items","count","limit","truncated","next_cursor","window"}`;
  `ndjson` prints one item per line. `logs -f` prints NDJSON in both JSON
  formats.
- Numbers: `1.2k`, `3.4M`, `2G`; sizes in decimal units (`1.5 MB`), as the
  config files write them; durations `12.3ms`, `2.50s`.
- Times are **UTC**, in the CLI and in the TUIs.
- Text from events, spans and logs is cleaned: control and bidi characters
  are shown as escapes (`\u{1b}`) and never reach the terminal. JSON
  escapes them as `\uXXXX`.
- Errors: one line on stderr, `error: <what went wrong> (<what to try>)`;
  with `--format json|ndjson` one JSON line
  `{"error":{"code","message","status","retryable","hint"}}`. Output into a
  closed pipe (`… | head`) is not an error.

Exit codes: 0 ok, 1 internal, 2 usage, 3 auth, 4 not found, 5 conflict,
6 unavailable (retry), 7 disabled on the server, 8 condition not met.

## TUI keys

These keys mean the same on both tabs and every screen (the list is
`mallard_term::tui::keys::SHARED`; `?` shows it at the top of the help):

| key | action |
|---|---|
| `↑↓` `j k` `pgup pgdn` `g G` | move |
| `↵` | open |
| `esc` `q` | back (`q` on the first screen: quit) |
| `tab` `shift-tab` `1-9` | the tabs of this screen (errors: status or section; telemetry: views) |
| `/` | filter or search |
| `t` `T` | next / previous time window |
| `r` | refresh now |
| `x` | the same trace on the other tab |
| `?` | help |
| `ctrl-c` | quit |
| `F1` `F2` | switch tab (`mallard tui`) |

Screen keys:

| tab | keys |
|---|---|
| errors, issue list | `s` sort, `p` project, `R` `I` `U` resolve / ignore / unresolve |
| errors, issue detail | `[` `]` older / newer event, `a` in-app frames, `w` raw frames, `R` `I` `U` |
| telemetry, traces | `E` errors only, `l` logs of the trace; `⚠ n`: Sentry errors of the trace or span, `x` on a marked span opens its error |
| telemetry, logs | `s` severity, `space` follow, `o` open the trace, `c` clear filter |
| telemetry, metrics | `a` `A` aggregation, `b` group by |

Writes (issue status) are capitals, so `r` can never change data.
