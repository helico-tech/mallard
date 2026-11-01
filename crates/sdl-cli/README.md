# sdl — CLI and TUI for sentry-ducklake

`sdl` talks to a sentry-ducklake server through its `/v1` API (via
`sdl-client`). It lists and changes issues, shows events with their stack
traces, sends test errors to a DSN, and has a live terminal dashboard.

```sh
cargo build -p sdl-cli          # binary: target/debug/sdl
export SDL_URL=http://localhost:8000 SDL_TOKEN=<SDL_API_TOKEN of the server>
```

## Global flags

| Flag | Env | Default | Meaning |
|---|---|---|---|
| `--url` | `SDL_URL` | `http://localhost:8000` | server base URL |
| `--token` | `SDL_TOKEN` | – | API token (the server's `SDL_API_TOKEN`) |
| `-p, --project` | `SDL_PROJECT` | only project | project id or slug |
| `--json` | | off | raw JSON output (for scripts) |

When the server has exactly one project, `-p` is optional.
Errors are one line on stderr with exit code 1, e.g.
`error: invalid token (set SDL_TOKEN or --token)` or
`error: cannot reach server at http://localhost:8000 (is it running? set SDL_URL)`.

## Commands

```
sdl info                                   server version, storage, queue depth
sdl projects list
sdl projects create <name> [--slug s] [--platform node]   prints the DSN
sdl projects show <project>                               DSN first, then details
sdl issues list [--status unresolved|resolved|ignored|all] [-q text] [--since 24h]
                [--sort last_seen|first_seen|count|users] [--env e] [--release r] [--limit n]
sdl issues show <id> [--since 24h]         details, releases, tag breakdown, activity
sdl issues resolve|ignore|unresolve <id>...
sdl events list [--issue id | --trace trace_id] [--limit n]
sdl events show <event_id> [--issue id]    exception chain, frames (newest first,
                                           in-app highlighted with source context),
                                           breadcrumbs, tags, user, request
sdl stats [--since 24h] [--interval 1h]    totals and events per bucket
sdl sourcemaps list                        uploaded artifact bundles
sdl maintenance                            flush, compact and expire now
sdl send-test --dsn <dsn> [--message m]    send a test error (Sentry envelope protocol)
sdl tui                                    interactive dashboard
```

`send-test` needs no token: it posts an envelope to
`{scheme}://{host}/{path}/api/{project}/envelope/` with `X-Sentry-Auth`, just
like an SDK, so it checks the DSN end to end.

```sh
sdl projects create "Web App" --platform node
sdl send-test --dsn http://KEY@localhost:8000/1
sdl issues list --since 1h
```

## TUI

```
 ◆ sentry-ducklake  Web App  web-app · node          ⠙ queue ■····· 12/20k  ● live  v0.1.0
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
 ↑↓ move  ⏎ open  tab status  / search  s sort  t window  p project  r/i/u …  ? help  q quit
```

Enter opens the issue detail with sections Overview (facts, trend, shown
event, activity, tag facets as bars), Stack trace (newest frame first,
in-app frames marked, source context of the selected frame with the error
line highlighted), Breadcrumbs, Events and JSON.

Data loads run in the background (tokio tasks), so input never waits on the
network. Everything refreshes every 5 s, the queue depth every 2 s.
The terminal is restored on exit and on panic. Below 50×12 it shows a
"terminal too small" note; columns and the stats panel drop out on narrow or
short terminals.

### Keys

| Where | Key | Action |
|---|---|---|
| everywhere | `↑ ↓` `j k`, `g G`, `PgUp PgDn` | move |
| | `R` | refresh now |
| | `?` | help |
| | `ctrl-c` | quit |
| issue list | `⏎` | open issue |
| | `tab` `← →` `1-4` | status tab (unresolved / resolved / ignored / all) |
| | `/` | search title and culprit (`esc` clears) |
| | `s` | cycle sort (last seen, first seen, events, users) |
| | `t` / `T` | cycle time window (1h, 24h, 7d, 14d, 30d) |
| | `p` | switch project |
| | `r` `i` `u` | resolve / ignore / unresolve |
| | `q` | quit |
| issue detail | `tab` `← →` `1-5` | section |
| | `[` `]` | older / newer event |
| | `a` | only in-app frames |
| | `w` | raw (unsymbolicated) frames, when the event has them |
| | `t` | error events of the shown event's trace (all projects) |
| | `⏎` (Events) | show that event |
| | `r` `i` `u` | change status |
| | `esc` `q` | back |

## As a library

The crate is also a library (`sdl_cli`) so other programs can mount it:

- `sdl_cli::Command` + `sdl_cli::run(cmd, &Ctx)`: the subcommands above.
  Global flags stay in the host; it passes `Ctx { client, project, json }`.
- `sdl_cli::Embedded`: the TUI as a component without terminal setup:
  `new(client, project)`, `draw(frame, area)` (draws only inside `area`),
  `handle_event(&Event) -> Outcome` (`Consumed`, `Ignored`, `Quit`,
  `OpenTrace(trace_id)`), `tick()` (call a few times per second; applies
  finished loads, never blocks) and `show_trace_errors(trace_id)`.
  F1 and F2 are always `Ignored` (reserved for the host). Needs a tokio
  runtime.

## Tests

`cargo test -p sdl-cli`: formatting helpers, event payload parsing, DSN and
envelope building, TUI state machine, and render tests with ratatui's
`TestBackend` for every view (including `Embedded` driven through
draw / handle_event / tick).
