# mallard-sentry-cli: the Sentry side of the CLI and TUI

The library behind `mallard`'s Sentry commands and the errors tab of its TUI. It has
no binary of its own. [docs/cli.md](../../docs/cli.md) is the reference for commands,
flags, output and exit codes; this page adds the details of this side.

`mallard` talks to the server through its `/sentry/v1` API (via
`mallard-sentry-client`). This side lists and changes issues, shows events with their stack
traces, sends test errors to a DSN, and has a live terminal dashboard.

```sh
cargo build -p mallard          # binary: target/debug/mallard
export MALLARD_URL=http://localhost:8080 MALLARD_TOKEN=<one of the server's server.api_tokens>
```

## Notes on output

Global flags and env vars (`--url`, `--token`, `-p`, `--format`, ...): see
[docs/cli.md](../../docs/cli.md) and ADR 0006, 0008.

When the server has exactly one project, `-p` is optional. The bridge creates a
project per service for OTel exceptions (`bridge.default_project`), so a server with
OTel traffic can need it. When a project already has that slug under the slug rule (`my_app` for
`my-app`), the bridge uses it (ADR 0031).

Text that comes from events (titles, messages, frames, tags, ...) or from
the server is cleaned before it is printed: control and bidi characters are
shown as escapes (`\u{1b}`), so an event can't send escape sequences to
your terminal. `--json` output keeps the data, with control and bidi characters
escaped as `\uXXXX`. Output to a closed pipe (`mallard issues list | head`) ends
quietly with exit 0. Errors are one line on stderr, with the exit codes of
[docs/cli.md](../../docs/cli.md), e.g.
`error: invalid token (set MALLARD_TOKEN or --token)` (3) or
`error: cannot reach server at http://localhost:8080/sentry (is it running? set MALLARD_URL)` (6).

## Commands

```
mallard status --json                          sentry.info: server version, storage, queue depth
mallard projects list
mallard projects create <name> [--slug s] [--platform node] [--if-not-exists]   prints the DSN
mallard projects show <project>                               DSN first, then details
mallard issues list [--status unresolved|resolved|ignored|all] [-q text] [-s 24h]
                [--sort last_seen|first_seen|count|users] [--env e] [--release r] [-n limit]
                [--new] [--first-release r] [--tag key=value]... [--cursor c]
mallard issues show <id> [-s 24h]              details, releases, tag breakdown, activity
mallard issues resolve|ignore|unresolve <id>... [--reason text]
mallard events list [--issue id | -t trace_id] [-n limit] [-s 1h] [--tag key=value]...
                [--release r] [--env e] [-q text] [--cursor c] [--wait 30s]
mallard events show <event_id> [--issue id]    exception chain, frames (newest first,
                                           in-app highlighted with source context),
                                           breadcrumbs, tags, user, request
mallard stats [-s 24h] [--interval 1h]         totals and events per bucket
mallard releases list [-s 14d] [-n limit]      releases seen in events (derived; not sentry-cli's)
mallard sql --errors "<select>" [-n limit]     read-only SQL over events, issues, issue_activity
                                           (server: sentry.query.enable_sql; exit 7 when off)
mallard sourcemaps list                        uploaded artifact bundles
mallard flush                                  flush and compact now, on the server
mallard send-test --dsn <dsn> [--message m]    send a test error (Sentry envelope protocol)
mallard tui                                    terminal UI; F1 is this side
```

`send-test` needs no token: it posts an envelope to
`{scheme}://{host}/{path}/api/{project}/envelope/` with `X-Sentry-Auth`, just
like an SDK, so it checks the DSN end to end.

```sh
mallard projects create "Web App" --platform node
mallard send-test --dsn http://KEY@localhost:8080/sentry/1
mallard issues list --since 1h
```

## TUI

```
 ◆ mallard  Web App  web-app · node          ⠙ queue ■····· 12/20k  ● live  v0.1.0
╭ Overview last 24h ─────────────────────────╮╭ Events last 24h · peak 18 per 30m ─────────╮
│   EVENTS    ISSUES     NEW      USERS      ││          ▂▂█        ▂▂█         ▂▂█     ▂ │
│    1.3k       3         1        60        ││  ▂▂▅▅██████   ▅▅██████   ▂▂▅▅██████  ▅▅██ │
│   53.5/h  with events first seen affected  ││-24h                                   now │
╰────────────────────────────────────────────╯╰───────────────────────────────────────────╯
  Unresolved 3   Resolved   Ignored   All                  / pay  sort Last seen ▾  window 24h
╭ Issues unresolved ─────────────────────────────────────────────────────────────────────╮
│ LEVEL   ISSUE                                     EVENTS USERS TREND        LAST SEEN  │
│▌ ERROR  TypeError: Cannot read properties of…       1.2k    56 ▁▁▁▁▁▁▁▁▁▁▅█ 3m ago     │
│         app/checkout.js in parseConfig                                                 │
│  FATAL  Error: inside span                            42     3 ▁▁▁▁▁▁▁▁▁▁▅█ 1h ago     │
╰────────────────────────────────────────────────────────────────────────────────────────╯
 ↑↓ move  ⏎ open  tab status  / search  s sort  t window  p project  R/I/U …  ? help  q quit
```

Enter opens the issue detail with sections Overview (facts, trend, shown
event, activity, tag facets as bars), Stack trace (newest frame first,
in-app frames marked, source context of the selected frame with the error
line highlighted), Breadcrumbs, Events and JSON.

Data loads run in the background (tokio tasks), so input never waits on the
network. Everything refreshes every 5 s, the queue depth every 2 s.
The terminal is restored on exit and on panic; a panic in a background load
ends `mallard tui` with an error message. A failed load is shown where its data
would be (issue list, stats, issue, event, trace); the header says
"offline" only when the server can't be reached. `mallard tui` needs a terminal
(stdin and stdout must be a TTY). Below 50×12 it shows a
"terminal too small" note; columns and the stats panel drop out on narrow or
short terminals.

### Keys

The keys every tab shares (`?` lists them first) are in
[docs/cli.md](../../docs/cli.md): move, `↵` open, `esc`/`q` back, `/`
search, `t`/`T` time window, `r` refresh, `x` same trace on the other tab,
`?` help, `ctrl-c` quit. Ctrl/alt + key is ignored (only plain keys run
actions), except `ctrl-c` / `ctrl-q` quit and `ctrl-u` clears the search.

| Where | Key | Action |
|---|---|---|
| issue list | `tab` `← →` `1-4` | status tab (unresolved / resolved / ignored / all) |
| | `s` | cycle sort (last seen, first seen, events, users) |
| | `t` / `T` | cycle time window (1h, 24h, 7d, 14d, 30d; in `mallard tui` one window for both tabs) |
| | `p` | switch project (in `mallard tui`: choose the app) |
| | `/` | search; `key=value` is a tag filter, `tab` accepts a suggestion |
| | `R` `I` `U` | resolve / ignore / unresolve |
| issue detail | `tab` `← →` `1-5` | section |
| | `[` `]` | older / newer event |
| | `a` | only in-app frames |
| | `w` | raw (unsymbolicated) frames, when the event has them |
| | `x` | error events of the shown event's trace (`mallard tui`: the trace on the telemetry tab) |
| | `⏎` (Events) | show that event |
| | `R` `I` `U` | change status |

## As a library

`crates/mallard/src/main.rs` mounts the crate (`mallard_sentry_cli`) like this:

- `mallard_sentry_cli::Command` + `mallard_sentry_cli::run(cmd, &Ctx)`: the subcommands above.
  Global flags stay in the host; it builds
  `Ctx::new(client).with_project(p).with_json(j).with_color(c).with_hints(h)`.
  `Hints` (from `mallard-term`) names the command and env vars in messages
  (default `mallard_sentry_cli::HINTS`: `mallard …`, `MALLARD_URL`, `MALLARD_TOKEN`,
  `MALLARD_PROJECT`). Errors keep the `mallard_sentry_client::Error` as their source, so a
  host can downcast them; end with `mallard_term::error::report`.
- `mallard_sentry_cli::Embedded`: the TUI as a component without terminal setup:
  `new(client, project)` (optionally `.with_hints(h)`), `draw(frame, area)`
  (draws only inside `area`), `handle_event(&Event) -> Outcome`
  (the shared `mallard_term::tui::Outcome`: `Consumed`, `Ignored`, `Quit`,
  `Jump(trace_id)`, `JumpEvent { trace_id, event_id }`), `tick()` (call a few
  times per second; applies finished loads, never blocks), `take_fatal()`,
  `open_trace(trace_id)` and `open_trace_event(trace_id, event_id)` (the
  trace's events, then that event's issue).
  F1 and F2 and ctrl/alt keys (other than `ctrl-c`/`ctrl-q`, which return
  `Quit`) are `Ignored`.
  **Runtime requirement:** call `Embedded::new` inside a tokio runtime
  context (from async code, or under `Runtime::enter`); it panics otherwise.
  It keeps that runtime's handle and spawns loads on it, so `draw`,
  `handle_event`, `tick` and `open_trace` may then be called from any
  thread. Dropping it cancels the loads still running.
  **Internal errors:** a panicking load is caught; `take_fatal()` returns
  its message once (check it after `tick`). The panic hook has already run,
  so the host should restore the terminal and exit with that error.

## Tests

`cargo test -p mallard-sentry-cli`: formatting helpers, event payload parsing, DSN and
envelope building, TUI state machine, and render tests with ratatui's
`TestBackend` for every view (including `Embedded` driven through
draw / handle_event / tick).
