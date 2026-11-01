# mallard CLI and TUI

One binary, two sides that look and behave alike (ADR 0006). The shared
pieces live in `crates/mallard-term`.

## Commands

| command | what |
|---|---|
| `mallard serve -c mallard.toml` | run the server |
| `mallard check -c mallard.toml` | validate a config file |
| `mallard status` | health of both sides |
| `mallard trace <trace-id> [-s 7d]` | spans, Sentry errors and logs of one trace |
| `mallard errors …` | Sentry side: `info`, `projects`, `issues`, `events`, `stats`, `sourcemaps`, `maintenance`, `send-test`, `tui` |
| `mallard telemetry …` | OTel side: `health`, `status`, `services`, `ops`, `traces`, `trace`, `logs`, `metrics`, `metric`, `sql`, `tui` |
| `mallard tui [-p project]` | both TUIs as tabs: F1 errors, F2 telemetry |

`mallard errors …` is the `sdl` binary's command set and `mallard
telemetry …` is `odl`'s; the standalone binaries still work. Side details:
[crates/sdl-cli/README.md](../crates/sdl-cli/README.md),
[otel/cli.md](otel/cli.md).

## Shared flags

Global (any position):

| flag | env | meaning |
|---|---|---|
| `--url` | `MALLARD_URL` (`SDL_URL`, `ODL_URL`) | server base URL |
| `--token` | `MALLARD_TOKEN` (`SDL_TOKEN`, `ODL_TOKEN`) | API token |
| `--json` | | JSON instead of tables (`odl -o json` still works) |
| `--color auto\|always\|never` | `NO_COLOR` | `auto`: colour only on a terminal, without `NO_COLOR` and with `TERM` not `dumb` |
| `-p, --project` | `MALLARD_PROJECT` (`SDL_PROJECT`) | errors side only; optional with one project |

Per command, the same letter for the same thing on both sides:

| flag | meaning |
|---|---|
| `-s, --since 1h` | time window ending now (`--from`/`--to` RFC 3339 on the telemetry side) |
| `-n, --limit N` | max rows (`-l` still works for `telemetry traces`) |
| `-q, --query text` | text search |
| `-t, --trace id` | only this trace (`errors events list`, `telemetry logs`) |
| `-S, --service name` | only this service (telemetry) |

Defaults differ where the data does: the telemetry window is `1h`, the
errors window is the server's (14 days for issues).

## Output

- Tables: one style (rounded, bold headers).
- Numbers: `1.2k`, `3.4M`, `2G`; sizes in decimal units (`1.5 MB`), as the
  config files write them; durations `12.3ms`, `2.50s`.
- Times are **UTC**, in the CLI and in the TUIs.
- Text from events, spans and logs is cleaned: control and bidi characters
  are shown as escapes (`\u{1b}`) and never reach the terminal. `--json`
  escapes them as `\uXXXX`.
- Errors: one line on stderr, `error: <what went wrong> (<what to try>)`,
  exit code 1. Output into a closed pipe (`… | head`) is not an error.

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
| telemetry, traces | `E` errors only, `l` logs of the trace |
| telemetry, logs | `s` severity, `space` follow, `o` open the trace, `c` clear filter |
| telemetry, metrics | `a` `A` aggregation, `b` group by |

Writes (issue status) are capitals, so `r` can never change data.
