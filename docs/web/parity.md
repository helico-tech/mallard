# Web UI and TUI parity

State on 2026-11-01. Epic 0004 changed the app scope and the window; rows below say so, and since story 0004/03 the TUI follows the same rules (`mallard tui`; the errors and telemetry tabs on their own keep their earlier keys and windows).

The TUI source is the reference (`crates/mallard/src/tui.rs`, `crates/sdl-cli/src/tui/`,
`crates/odl-cli/src/tui/`, `crates/mallard-term/`). The web UI was driven with the
keyboard in Playwright (Chromium, `timezoneId: Asia/Tokyo`) on the Vite dev server,
against a server with seeded data. Requests were recorded and compared with what
the TUI sends. Format functions were compared with a small program that calls the
real `mallard_term` code (133 cases).

Status:

- **ok**: same behaviour.
- **intended**: a chosen difference.
- **differs**: both have it, it behaves differently, and no decision was made.
- **missing**: the web UI does not have it.
- **bug**: broken in the web UI.

"(code)" means checked by reading the code only. "(mock)" means the data was
changed in the browser, because the seeded data has no such case.

## All screens

| Feature | TUI | Web | Status |
|---|---|---|---|
| Keys while a toast shows | Never blocked | Never blocked | ok |
| Keys after a picker closes | Work | j, k, `s`, `?`, F1/F2 work | ok |
| Enter and ↓ after a picker closes | Act on the row | Act on the row: a closed menu gives the keys back | ok |
| F1 / F2 | Switch tab, each tab keeps its screen, filters, selection | Same | ok |
| `1`-`5` on telemetry | Switch view, each view keeps its filter and open detail | Same | ok |
| Back after F1/F2 or `1`-`5` | Esc goes up one screen | Esc goes up one screen: each side keeps its own back stack (`app/history.ts`) | ok |
| Back with `t` or `b` used on the screen | Esc goes up one screen | Esc goes up one screen and the window stays | ok |
| Esc after `x` | Trace list or events list of the other tab | Same: each side keeps its own back stack | ok |
| Esc on a direct link | Goes up one screen | Same: goes to the list above | ok |
| `?` help | Shared keys, screen keys, filter language, UTC | Same | ok |
| Closing help | Any key | Only Esc, or the close button | intended |
| Footer hints per screen | Always shown | Help and tooltips instead | intended |
| `r` refresh | Reloads the current screen, also log records | Same | ok |
| `r` flash | "refreshing…" on telemetry | None (toast on a trace only) | intended |
| `t` `T` presets | The union, 5m to 30d, cut to the smaller `max_window_secs` of the two sides; wrap around | Same (story 0004/04) | ok |
| Default windows | 1h; an empty issue list offers `d`: show last 24h | 1h; an empty issue list offers "Show last 24h" (story 0004/04) | ok |
| Window across F1/F2 | One window for both sides (ADR 0028) | Same | ok |
| Window across `1`-`5` | One window for all views | One window for all views | ok |
| `t` flash | None; the top bar shows the window | None | ok |
| `t` on issue detail | Changes the window | Changes the window | ok |
| `t` on trace detail and Status | Cycles the window | Does nothing, no window control there | intended |
| UTC everywhere | Yes | Yes in Tokyo: tables, facts, tooltips, title attributes, hover labels, chart axes | ok |
| Text cleaning | `clean` / `clean_text` | Same output on 6 hostile strings | ok |
| Failed refresh keeps data | Yes, says so | Yes on issues, traces, overview, logs, with a note that clears on recovery | ok |
| Live state, errors tab | `● offline` after a failed load | "Offline" after the failed load and its one retry (within about 3 s), "Live" on recovery | ok |
| Live state, telemetry tab | live under 20 s, then stale; offline if never loaded | Same | ok |
| Header, errors tab | Project, slug, platform; queue always; version; URL | Queue only above 0; no platform; URL and version in the side nav | **differs** |
| Header, telemetry tab | Ingest rate | Shown (`x/s ingest`) | ok |
| Info poll | Every 2 s | Every 2 s | ok |

## Number and time formats (`mallard_term::fmt`)

| Function | TUI | Web | Status |
|---|---|---|---|
| `compact` | `1.2k`, `1M`, `2.50`, `0.250` | Same; only 100.5 rounds differently (100 vs 101) | ok |
| `bytes` | `1.0 kB`, `512.0 MB`; stops at TB | Same; from 1 PB it says `5.0 PB`, not `5000.0 TB` | ok |
| `ms` | `500µs`, `12.3ms`, `2.50s`, `1.5m` | Same; only 0.0005 rounds differently | ok |
| `relative_time` | `now` under 5 s, then s m h d mo y | Same on all 17 cases | ok |
| `age` | `now` under 5 s | Same | ok |
| `timestamp` | `2026-10-31 20:59:13` | Same | ok |
| `uptime`, time formats per window | `%m-%d %H:%M:%S` over 24h, else `%H:%M:%S%.3f` | Same | ok |
| Severity names | `ERR…` text is ERROR; number over 24 falls back to the text | Text must start with `ERROR`; over 24 gives `-` (code) | **differs** |

## Errors: issue list

| Feature | TUI | Web | Status |
|---|---|---|---|
| Move: j k ↑↓ PgUp PgDn g G Home End | PAGE = 10 | Same | ok |
| Status: `1`-`4`, `h l ← →` | Yes | Yes | ok |
| `s` sort: last seen, first seen, events, users | Yes | Yes | ok |
| `/` search, Enter, Esc cancels, Esc clears | Yes | Yes | ok |
| After Enter in search | Prompt closes, keys act on the list | The box gives up focus, keys act on the list | ok |
| `Ctrl-U` clear in search | Yes | Not bound (code) | **differs** |
| Requests | `issues?status&since&sort&limit=200`, `stats?since&interval` | Same | ok |
| Refresh cadence | 5 s | 5 s | ok |
| Selection kept by id on refresh | Yes | Yes (mock: reversed list); row gone: nearest index | ok |
| Selection after a filter change | Goes to the old top row | Stays on the same issue | **differs** |
| Columns | LEVEL ISSUE EVENTS USERS TREND LAST-SEEN AGE STATUS | Same, STATUS when the card is wide enough | ok |
| KPI tiles and peak | Events x/h, issues, new, users; "peak n per 30m" | Same values; "Events per 30m" and "peak n" | ok |
| R I U on the list | Optimistic status in the row, toast, PATCH, reload | Same; rollback with a toast on error | ok |
| R when already that status | Sends PATCH, toast | Toast "Already resolved", sends nothing | intended |
| Empty state | "✓ No unresolved issues matching "x" in the last 7d" + hint | Same words, with buttons | ok |
| No projects, unknown project | Hint with the command; error | Same idea | ok |
| `p` picker | One app picker, on every screen, scoping both sides (ADR 0028): j k ↑↓ Enter; Esc q p close | One app picker in the top bar, on every screen, scoping both sides (ADR 0028): Kumo menu, arrows, Enter, Esc | intended |
| Errors with All apps | Asks to pick an app: issues are per project (ADR 0028) | Same | ok |
| A side the app lacks | Says so, with `mallard setup <app>` | Same | ok |
| Filter suggestions | The list shows while typing; Tab takes the selected row, the first unless you moved; ↑↓ choose; ↵ applies what is typed | The list opens while typing or on ↓; nothing is taken until you move to it, then Enter or Tab takes it. Otherwise Tab moves the focus, as in a browser | intended |
| Issue search | Text, and `tag=value` | Same | ok |
| Cmd-K, app home page | None | Web only (stories 0004/04, 05) | intended |
| Project picker reloads projects | Yes | Only with `r` (code) | **differs** |
| Project switch | Toast "switched to X", keeps filters | Same | ok |

## Errors: issue detail

| Feature | TUI | Web | Status |
|---|---|---|---|
| Esc q Backspace back | Back to the list as it was | Same: status, sort, search and row kept | ok |
| Sections `1`-`5`, `h l ← →` | Yes | Yes | ok |
| Scroll on Overview and JSON: j k PgUp PgDn Space g G Home End | Yes | Yes | ok |
| Requests | `issues/{id}?since`, `events?issue&limit=50`, `events/{id}?issue` | Same | ok |
| Refresh cadence | Issue every 5 s, events and event once | Same | ok |
| Overview facts | Status to last release, trend, shown event, contexts, request, activity, tags | All there | ok |
| Overview Events and Users | Exact number | Compact | **differs** |
| R I U on detail | Optimistic, toast, activity updates | Same; failed write rolls back with a toast | ok |
| `x` | Jump to the trace; toast when none | Same; also passes a time hint (±1 h) | ok |
| `[` `]` | Older / newer, next page past the end, toasts at both ends | Same (across pages) | ok |
| Stack: default frame | First in-app | Same on open | ok |
| Stack: j k PgUp PgDn g G | Yes | Yes | ok |
| Stack: exception chain, Caused by, mechanism, context | Yes | Yes (mock: seeded events have no chain) | ok |
| `a` `w` toggles | Kept per issue, on any section | Same (`w` on raw frames: mock) | ok |
| Frame after `a` `w` | Resets to the first in-app frame | Stays when the first in-app row keeps its id: G, `a` stays on the last frame | **differs** |
| `w` with no raw frames | Toast | Same | ok |
| Event with a message and no stack | Message shown | Same | ok |
| Breadcrumbs table, j k g G | Yes | Yes | ok |
| Events: move | Yes | Yes | ok |
| Events: Enter shows that event on Stack | Yes | Same, also by clicking the id | ok |
| Events: last row | Loads the next page | Same, until the end of the list (124 of 124) | ok |
| JSON view | Full payload with line numbers and colours | Same | ok |

## Errors: events of a trace

| Feature | TUI | Web | Status |
|---|---|---|---|
| Columns and call | LEVEL PROJECT EVENT SPAN WHEN; `events?trace_id` | Same, plus Id | ok |
| Enter opens the event | Yes | Yes | ok |
| Esc from an event opened here | Back to this list | Same | ok |
| Esc on a direct link | Issue list | Same | ok |
| `x` | Ignored | Opens the trace | **differs** |

## Telemetry: Overview

| Feature | TUI | Web | Status |
|---|---|---|---|
| Tiles | requests, errors, p95, logs, ingest buffer | Same values | ok |
| Charts | Throughput, Latency, same time axis | Both, but Latency stops at the last bucket with data (code, screenshot) | **differs** |
| Services table: move, Enter, `→`, `l` | Yes | Yes | ok |
| Services table: long names | Name truncates, numbers stay | Same | ok |
| Operations: move, Enter, Esc Bksp ← h q | Yes | Yes | ok |
| Enter on an operation | Traces filtered by service and op | Same | ok |
| Cadence | 5 s | 5 s | ok |

## Telemetry: Traces list

| Feature | TUI | Web | Status |
|---|---|---|---|
| Columns | start, root, duration, bar, spans, errors, sentry, services | Same | ok |
| Call | `limit=200, since, service, operation, errors_only, min/max, attributes` | Same | ok |
| Load more | n/a | Next 200 with cursor, from and to; a new window starts again at 200 | intended |
| Move, Enter, `→`, `l`, `x` | Yes | Yes | ok |
| `/` and `f`, tokens, quotes, `>200`, attributes | Yes | Yes | ok |
| Bad filter | Flash, old filter stays, prompt closes | Inline message, old filter stays, box stays open | **differs** |
| `E` with toast | Toggles errors-only | Same, also for `error=on` | ok |
| `c` clear | Yes | Yes | ok |
| Parser details | Any whitespace; `.5s` ok; keys case-sensitive | Spaces only; `.5s` refused; keys any case (code) | **differs** |
| Cadence | 5 s | 5 s | ok |
| Sentry column refresh | One lookup after every list load | Every 5 s with the list | ok |
| Sentry lookup fails | Flash, keeps last markers | Keeps last markers; `–` if none yet; no message | **differs** |
| Selection by id | Yes | Yes (mock: reversed list) | ok |

## Telemetry: Trace detail

| Feature | TUI | Web | Status |
|---|---|---|---|
| Calls | trace, logs `limit=2000&order=asc`, ±1 h with a hint, else 7d, then 24h | Same; by-trace from first span -1 h to end +1 h | ok |
| No periodic refresh, `r` reloads | Yes | Yes | ok |
| j k g G Home End, PgUp PgDn `[` `]` scroll | Yes | Yes | ok |
| Esc Bksp h ← q | Back to the trace list | Same from the list; follows history otherwise (see All screens) | ok |
| `x` on a span with an event | Opens that event on Stack | Same | ok |
| `x` on another span | Events of the trace | Same | ok |
| `l` | Logs of the trace in the current window | Same | ok |
| Span events offset | `+x` from trace start | Same (non-zero offset: mock; seeded events sit at 0) | ok |
| Scope, links, Sentry event id | `name version`; full ids | Name only; link 16 chars; event id 12 chars | **differs** |
| Header facts | Duration, spans, services, ✗, ⚠ sentry, logs | Same, as text and buttons | ok |
| Marks on spans | ⚠ | Same | ok |
| Trace not found | Shows the error, retries with back-off | "This trace has not arrived yet", retries with back-off | ok |
| Not a trace id | Flash | Page says so | ok |

## Telemetry: Logs

| Feature | TUI | Web | Status |
|---|---|---|---|
| Calls | `limit=1000&order=desc`, tail `from`, then `since=…ms`, `limit=500`; series 5 s; tail 2 s | Same | ok |
| Tail start | Newest record's time | Time of the load (code) | **differs** |
| Dedupe and append | Yes | Yes (mock) | ok |
| Follow sticks to the end; moved up stays | Yes | Yes | ok |
| Space, `F`, toast | Yes; back on selects the last row | Toggle and toast ok; selection not moved | **differs** |
| Paused: no polls | Yes | Yes | ok |
| Start state | Last row selected, no detail | Same | ok |
| `o` | Opens the trace or says none | Same | ok |
| Enter | Opens and closes detail | Opens and closes detail | ok |
| Detail keys | Esc Enter q close, selection kept; j k scroll; `o` `t` open trace | Esc and `q` close, selection kept; j k move rows (split pane) | intended |
| `s` severity cycle and toast | all, info, warn, error; by number (`debug`→warn, `fatal`→all, `14`→error) | Same | ok |
| `/` filter, bad level, `c`, Esc clears | Yes | Yes | ok |
| `r` | Clears and reloads the records | Same | ok |
| Volume title and legend | "service and text filter only" | Same | ok |
| Time format, 400 char cut | Yes, with `…` | Yes, no `…` | ok |
| Note after a failed tail | Cleared by the next good poll | Same | ok |
| Gap row | Only with new records | Also with none (code) | **differs** |

## Telemetry: Metrics

| Feature | TUI | Web | Status |
|---|---|---|---|
| List columns, 30 s cadence | Yes | Yes | ok |
| `/` live search, Esc clears, Enter, `→` | Yes | Yes | ok |
| Back from chart keeps search | Yes | Yes | ok |
| `a` `A` `→`, aggregations per type | Yes | Yes | ok |
| `b` group by, kept across `a` | Yes | Yes | ok |
| Esc Bksp ← h q | Yes | Yes | ok |
| Chart cadence | 10 s | 10 s | ok |
| Value formats by unit | `1`, `s`, `ms`, `By` | Same rules; percent check uses signed max (code) | **differs** |
| Reload or direct link of a chart | n/a | Works: a path outside `/assets/` is a screen of the app | ok |

## Telemetry: Status

| Feature | TUI | Web | Status |
|---|---|---|---|
| Ingest and storage tables | Yes | Yes | ok |
| Ingest rate | In the panel title | In the header | intended |
| Snapshot id | Exact | Exact | ok |
| Extra "What the server allows" | n/a | Added | intended |

## Not checked

- The TUI was not run next to the web UI; it is read from source.
- The production binary with these changes: no CSP, and no `asset` route for direct links.
- A real server outage: failures were simulated by blocking requests.
- Gap row and tail start in a real burst, or after a pause over 10 minutes.
- F1 in a real, non-headless browser.
- Widths under 1700 px.
- Login mode.
