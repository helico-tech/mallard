# mallard CLI and TUI

`mallard web` is the Web UI, with the same screens and keys: see
[crates/mallard-web/README.md](../crates/mallard-web/README.md).

The `mallard` binary: server, CLI and TUI (ADR 0023). The CLI and TUI have two
sides that look and behave alike (ADR 0006). The shared
pieces live in `crates/mallard-term`.

## Commands

One flat tree (ADR 0008). For AI agents: [agents.md](agents.md), also
`mallard help agents`. Every command's `--help` has examples.

| command | what |
|---|---|
| `mallard status` | health of both sides and what the server allows (SQL, windows, retention); exit 6 when a side is down |
| `mallard setup <name> [--platform p] [--sentry-cli]` | get or create the project (one with the same slug under the slug rule counts: `my_app` for `my-app`; with `--slug my-app` that project is a conflict, exit 5); print `SENTRY_DSN` and the `OTEL_EXPORTER_OTLP_*` env vars; `--sentry-cli` adds `SENTRY_URL` (the server root), `SENTRY_ORG`, `SENTRY_PROJECT`, `SENTRY_AUTH_TOKEN` |
| `mallard apps [--since 24h]` | every app: a project and the services with the same slug, or either alone (ADR 0027). Projects are listed whatever the window; services need data in it |
| `mallard find <id>` | a trace or event id (32 hex) or an issue or span id (16 hex): what it is and the command to see more |
| `mallard issues list\|show\|resolve\|ignore\|unresolve` | error issues; `list --new`, `--first-release`, `--tag k=v` (repeat), `--cursor`; status changes take `--reason` |
| `mallard events list\|show` | error events; `list --tag k=v`, `--release`, `--env`, `-q`, `--cursor`; `show --json` is concise, `--raw` adds the Sentry payload |
| `mallard releases list` | releases seen in error events: first/last seen, events, issues, new issues (derived from events; releases made with `sentry-cli releases new` are not stored) |
| `mallard projects list\|show\|create [--if-not-exists]` | Sentry projects and their DSNs. A new slug that another project has under the slug rule is a conflict, exit 5, also with `--if-not-exists` |
| `mallard traces list\|show`, `mallard trace <id>` | search traces (with a `sentry` error count per trace; `--cursor` for the next page); one trace with its spans, Sentry errors (`⚠ n` on the span) and logs |
| `mallard logs` | search (`-S`, `-l`, `-q`, `-t`, `-a`; `--cursor` for the next page), follow (`-f`, stops by itself with `--for 2m` or `--until-match <text>`), or `--around <time> --window 1m` |
| `mallard services`, `mallard ops` | request rate, error rate, latency per service / operation; `ops --compare 1d` adds p95 and error rate of the same window a day earlier, and the change |
| `mallard series spans\|logs\|errors` | counts per time bucket (`--step 5m`): spans with errors and p50/p95/p99, logs per severity band, Sentry events (errors, warnings, other) |
| `mallard metrics list\|show <name>` | metric names; one metric over time (`-g p95 -b http.route`; `--exemplars` adds trace ids of the highest samples) |
| `mallard sql "<select>" [--errors]` | read-only SQL over the telemetry tables, or with `--errors` over `events`, `issues`, `issue_activity` (exit 7 when off; ADR 0011; `-n` rows and 32 MiB per answer, ADR 0015) |
| `mallard schema [tables]`, `mallard schema attrs` | the tables and columns `sql` can query, per `side` (`errors` tables need `sql --errors`); attribute keys with counts, or the top values of `--key` (`--signal spans\|logs\|metrics`, `-S`, `--metric`; sampled) |
| `mallard sourcemaps list`, `mallard send-test` | uploaded bundles; send a test error to a DSN |
| `mallard tui [-p project \| --app slug]` | both TUIs as tabs, one app and one time window: F1 errors, F2 telemetry |
| `mallard web [--addr a]` | the Web UI on `127.0.0.1:8088`: the same screens and keys in a browser |

The server commands read the config from `-c/--config` or `MALLARD_CONFIG`, put
after the command (`mallard serve -c mallard.toml`); `--url` and `--token` do
not apply to them. A build with `--no-default-features` has all of the above but
not these: there they are a usage error (exit 2) that says so.

| command | what |
|---|---|
| `mallard serve` | run the server until Ctrl-C or SIGTERM; logs go to stderr |
| `mallard check [--connect]` | validate the config (a bad one is exit 2); `--connect` also opens the catalog and storage, and builds the sourcemap store (a bad `artifacts_path` is exit 2) |
| `mallard maintenance [--full]` | both sides' maintenance once, then exit; stop the server first with a DuckDB-file or SQLite catalog |
| `mallard install-extensions --dir <dir>` | download all DuckDB extensions, for running without internet (the Docker image does this) |

The old paths still work, hidden: `mallard errors …` (the Sentry command
set: `info`, `stats`, `maintenance`, …) and `mallard telemetry …` (the telemetry
one: `health`, `status`, `metric`, waterfall `trace`, …). Side details:
[crates/mallard-sentry-cli/README.md](../crates/mallard-sentry-cli/README.md), [otel/cli.md](otel/cli.md).

## Shared flags

Global (any position):

| flag | env | meaning |
|---|---|---|
| `--url` | `MALLARD_URL` | server base URL |
| `--token` | `MALLARD_TOKEN` | API token |
| `-p, --project` | `MALLARD_PROJECT` | Sentry project id or slug; optional with one project. Only on `issues`, `events`, `releases`, `sourcemaps`, `find`, `series errors`, `tui`: elsewhere, and with `events list --trace`, the flag is exit 2 and the env var is ignored |
| `--app slug` | | an app (`mallard apps`): its project and its services, in place of `-p` and `-S` |
| `--format text\|json\|ndjson` | `MALLARD_FORMAT` | output format; `--json` = `--format json` |
| `--fields a,b` | | keep only these top-level fields (unknown field: exit 2) |
| `--raw` | | include raw payloads (Sentry `payload`, resource attributes) |
| `--color auto\|always\|never` | `NO_COLOR` | `auto`: colour only on a terminal, without `NO_COLOR` and with `TERM` not `dumb` |
| `--quiet` | | no `note:` lines on stderr (truncation, waiting, fallbacks); data, warnings and errors still print. No `-q`: that is `--query` |

Per command, the same letter for the same thing on both sides:

| flag | meaning |
|---|---|
| `-s, --since 1h` | time window ending at `--to` or now |
| `--from`, `--to` | RFC 3339 or a duration ago (`--from 2h --to 1h`) |
| `-n, --limit N` | max rows (`-l` still works for `traces list`) |
| `-q, --query text` | text search |
| `-t, --trace id` | only this trace (`events list`, `logs`) |
| `-S, --service name` | only this service (telemetry) |
| `--cursor c` | the next page: the `next_cursor` of the previous one (`issues list`, `events list`, `traces list`, `logs`) |
| `--wait 30s` | poll until there is a result (`events list`, `traces list`, `logs`, `trace`, `find`); none in time: the empty result and exit 8 |

Default windows differ where the data does: issues 14d, events all (30d
with `--trace`), traces/logs/metrics 1h, `trace` and `find` 7d.

`--app <slug>` works where `-p` or `-S` do: `issues`, `events`, `releases`, `sourcemaps`,
`find`, `series errors` take the app's project; `traces list`, `logs`, `ops`, `metrics`,
`schema attrs`, `series spans|logs` take all its service names (ADR 0027, 0028). There is no
`app=` in any filter.

- An app with a project has that project's slug, also one like `my_app`: `_` and `-` count as
  the same when a project and a service are joined, so the service `my_app` joins it.
- It replaces `-p` and `-S`: giving one next to it is exit 2. `MALLARD_PROJECT` is only a
  default and gives way to the flag. An empty `--app ""` means no app.
- Everywhere else it is exit 2, and so is `events list --trace`, which looks in all projects.
- A side the app lacks, or an unknown app, is exit 4 with what to do, never an empty list
  that looks like "no errors". An app's services are those with data in the command's window
  or the last 24 hours. So `--wait` does not wait for an app's first data; use `-S` for that.
- If the list of apps had warnings and the side is not in it, that is exit 6, because the side
  may only be unreadable. Otherwise the warnings go to stderr and the command runs.

## Output

- stdout is data only; errors and notes go to stderr. Nothing prompts.
- Text on a terminal: tables in one style (rounded, bold headers),
  sparklines, colour. **Piped**: plain text, one row per line, columns two
  spaces apart, no boxes, sparklines or colour.
- JSON: pretty on a terminal, compact when piped. Lists are an envelope
  `{"items","count","limit","truncated","next_cursor","window"}`;
  `ndjson` prints one item per line. Where the server pages a list
  (`issues list`, `events list`, `traces list`, `logs`), `next_cursor` is
  set when more exist; pass it as `--cursor` with the same filters (for
  traces and logs also the `window` of the envelope; an issue cursor keeps
  its own window and sort). Text and ndjson print it in the note on stderr. `logs -f` prints NDJSON
  in both JSON formats. `logs -f --for <dur>` exits 0 when the time is up;
  `--until-match <text>` exits 0 after the first record whose body contains
  the text (case-insensitive), and with `--for` exits 8 when none came.
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
6 unavailable (retry), 7 disabled on the server, 8 condition not met
(`--wait` or `logs -f --until-match … --for …` saw nothing in time).

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
| `p` | choose the app, on every screen (`mallard tui`; `mallard errors tui`: switch project) |

Screen keys:

| tab | keys |
|---|---|
| errors, issue list | `s` sort, `R` `I` `U` resolve / ignore / unresolve; `d` the last 24h, on an empty list in a shorter window |
| errors, issue detail | `[` `]` older / newer event, `a` in-app frames, `w` raw frames, `R` `I` `U` |
| telemetry, traces | `E` errors only, `l` logs of the trace; `⚠ n`: Sentry errors of the trace or span, `x` on a marked span opens its error |
| telemetry, logs | `s` severity, `space` follow, `o` open the trace, `c` clear filter |
| telemetry, metrics | `a` `A` aggregation, `b` group by |

Writes (issue status) are capitals, so `r` can never change data.

### One app and one time window (`mallard tui`)

The top bar shows the app and the window of both tabs. `p` opens the picker. "All apps" is the
default: telemetry shows every service, and the errors tab asks you to pick an app, because
errors are per project. `-p` or `--app` is the first choice. An app without a side says so on
that tab, with what to do. Opening an event of another project moves the picker along.

`t` and `T` change the window on both tabs: 5m, 15m, 1h, 6h, 24h, 7d, 14d, 30d, up to the smaller
of the two sides' `max_window_secs`, starting at 1h. The errors tab on its own (`mallard errors
tui`) keeps its 24h start and its own list. An empty issue list in a window under 24h offers `d`:
show last 24h, so older issues do not look like no issues.

### Suggestions in a filter

The `/` filters of traces, logs and issues suggest keys, then values with counts. `tab` accepts,
`↑↓` choose, `↵` still applies what is typed. Typing never waits on the server: answers come in
the background, per window and app, and a failure is at most the small note "suggestions
unavailable".

| filter | keys | values from |
|---|---|---|
| traces | `service=`, `op=`, `errors`, `min=`, `max=`, span attributes | `/services` (inside an app: its services), `/operations`, `/attributes` |
| logs | `service=`, `level=`, `trace=`, log attributes | `/services` (inside an app: its services), the levels, `/attributes` |
| issues | tags of the project | `/projects/{p}/tags` |

In the issue filter, `key=value` is a tag filter and the other words search the text, as in the
web UI. A word with its `=` in quotes is text too: `"a=b"` searches for that text, while
`a="b c"` is a tag with a space in its value. (`issues list -q` is always text; use `--tag` for a
tag.) In an app, `service=` can only name one of its services: another is refused with a message
and nothing is read for it. `trace=` on logs follows the trace across apps, because it asks by id.
`op=` values come from at most the last 24h. Difference with the web UI, on purpose: there a
suggestion is taken only after you move to it; here the first one is ready for `tab`.

A name with control or bidi characters shows cleaned, in the list and in the filter, and `tab` puts
it in as the server has it, so the filter finds it.
