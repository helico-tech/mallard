# sdl — CLI and TUI for sentry-ducklake

> **Retired binary.** The standalone `sdl` binary is gone (story 0002/06);
> this crate is the library behind `mallard`'s Sentry commands (see
> [docs/cli.md](../../docs/cli.md)), with `MALLARD_URL`/`MALLARD_TOKEN`
> instead of `SDL_URL`/`SDL_TOKEN`.

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
| `--color` | `NO_COLOR` | `auto` | `auto`, `always` or `never` |

Flags, output style, time zone (UTC) and TUI keys are the same as on the
telemetry side; see [docs/cli.md](../../docs/cli.md) and ADR 0006.

When the server has exactly one project, `-p` is optional.
Text that comes from events (titles, messages, frames, tags, ...) or from
the server is cleaned before it is printed: control and bidi characters are
shown as escapes (`\u{1b}`), so an event can't send escape sequences to
your terminal. `--json` output
keeps the data, with control and bidi characters escaped as `\uXXXX`.
Output to a closed pipe (`sdl issues list | head`) ends quietly with exit 0.
Errors are one line on stderr with exit code 1, e.g.
`error: invalid token (set SDL_TOKEN or --token)` or
`error: cannot reach server at http://localhost:8000 (is it running? set SDL_URL)`.

## Commands

```
sdl info                                   server version, storage, queue depth
sdl projects list
sdl projects create <name> [--slug s] [--platform node]   prints the DSN
sdl projects show <project>                               DSN first, then details
sdl issues list [--status unresolved|resolved|ignored|all] [-q text] [-s 24h]
                [--sort last_seen|first_seen|count|users] [--env e] [--release r] [-n limit]
sdl issues show <id> [-s 24h]              details, releases, tag breakdown, activity
sdl issues resolve|ignore|unresolve <id>...
sdl events list [--issue id | -t trace_id] [-n limit]
sdl events show <event_id> [--issue id]    exception chain, frames (newest first,
                                           in-app highlighted with source context),
                                           breadcrumbs, tags, user, request
sdl stats [-s 24h] [--interval 1h]        totals and events per bucket
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
The terminal is restored on exit and on panic; a panic in a background load
ends `sdl tui` with an error message. A failed load is shown where its data
would be (issue list, stats, issue, event, trace); the header says
"offline" only when the server can't be reached. `sdl tui` needs a terminal
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
| | `t` / `T` | cycle time window (1h, 24h, 7d, 14d, 30d) |
| | `p` | switch project |
| | `R` `I` `U` | resolve / ignore / unresolve |
| issue detail | `tab` `← →` `1-5` | section |
| | `[` `]` | older / newer event |
| | `a` | only in-app frames |
| | `w` | raw (unsymbolicated) frames, when the event has them |
| | `x` | error events of the shown event's trace (`mallard tui`: the trace on the telemetry tab) |
| | `⏎` (Events) | show that event |
| | `R` `I` `U` | change status |

## As a library

The crate is also a library (`sdl_cli`) so other programs can mount it:

- `sdl_cli::Command` + `sdl_cli::run(cmd, &Ctx)`: the subcommands above.
  Global flags stay in the host; it builds
  `Ctx::new(client).with_project(p).with_json(j).with_color(c).with_hints(h)`.
  `Hints` (from `mallard-term`) names the host's command and env vars in
  messages (default `sdl_cli::HINTS`: `sdl …`, `SDL_URL`, `SDL_TOKEN`,
  `SDL_PROJECT`). Errors keep the `sdl_client::Error` as their source, so a
  host can downcast them; end with `mallard_term::error::report`.
- `sdl_cli::Embedded`: the TUI as a component without terminal setup:
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

`cargo test -p sdl-cli`: formatting helpers, event payload parsing, DSN and
envelope building, TUI state machine, and render tests with ratatui's
`TestBackend` for every view (including `Embedded` driven through
draw / handle_event / tick).
