# 0006 — One terminal style for both sides

## Status
accepted

## Context
ADR 0002 embeds the two libraries' CLIs and TUIs in `mallard`. They came
from two projects and still looked like two tools (review
`work/epics/0002-mallard/review/cross-cutting.md`, 2.M, 2.M2, 2.N, 2.O, 2.R):

- only the errors side cleaned untrusted text, and `mallard trace` used a
  third, weaker cleaner;
- two table crates (comfy-table 7 and 8), two colour rules, a
  process-global colour flag in `sdl-cli`;
- different flags for the same thing (`--json` vs `-o json`, `-l` vs `-n`,
  `--since` with and without `-s`), different error output;
- two TUI themes (256-colour vs true colour), conflicting keys (`r`
  resolved an issue on F1 but refreshed on F2; the jump to the other tab was
  `t` on F1 and `e` on F2 while `t` was the time window on F2), local time
  on F2 and UTC everywhere else;
- two `Outcome` enums and an adapter for them in `mallard`.

## Decision
A new crate `mallard-term` holds what both sides print with. Both CLI
libraries, both TUIs and the `mallard` host use it; the standalone `sdl`
and `odl` binaries too.

- `text::clean` / `clean_text` / `json_safe`: every control and bidi
  character becomes a visible escape (`\u{1b}`); one line unless asked
  otherwise. It merges the two earlier versions and keeps the stricter
  parts of each (bidi, one line, tabs as spaces).
- `color`: styling is always added; `Out::line` strips it when colour is
  off. `--color auto|always|never` on all three binaries; `auto` honours
  the TTY, `NO_COLOR` and `TERM=dumb`. No global state.
- `table::table`: one comfy-table (8) style.
- `fmt`: one number, size (decimal), duration and time format. **All times
  are UTC**, in CLIs and TUIs: the server stores UTC and `--from`/`--to`
  are RFC 3339, so screens and logs agree.
- `error`: `Hints` (command and env var names of the host), `describe` for
  both clients' failures, `report` for `error: …` + exit code 1, a closed
  pipe being success.
- `tui`: one 256-colour theme, `Outcome { Consumed, Ignored, Quit,
  Jump(trace_id) }`, small shared widgets, and the shared key map
  `tui::keys` (shown in both help screens and in `docs/cli.md`):
  `r` refresh, `t`/`T` time window, `/` filter, `x` same trace on the other
  tab, `?` help, `tab`/`1-9` the tabs of the current screen. Writes are
  capitals (`R`/`I`/`U`).

Same API shape on both sides: `Command`, `Ctx::new(client).with_json(…)
.with_color(…).with_hints(…)`, `describe`, `HINTS`, `Embedded`,
`Embedded::open_trace`. Both clients: `Client::new(url, Option<&str>)`,
the same error variants and timeout.

Not shared, on purpose: sparklines (counts start at zero, metric values
use min-max), per-screen keys, each side's domain colours (levels,
statuses, services), and anything server-side (ADR 0001, review 2.S).

## Consequences
- A change to output style, colours or keys is made once.
- Some keys and outputs changed for users: errors tab `r`→`R` (resolve),
  `i`/`u`→`I`/`U`, `R`→`r` (refresh), `t`→`x` (jump); telemetry tab `e`→`x`,
  `w`/`W` removed (use `t`/`T`); telemetry TUI times are UTC; numbers over
  1000 are compact (`1.2k`); sizes are decimal (`kB`) on both sides.
  Old CLI spellings stay as hidden aliases (`-o json`, `traces -l`).
- `mallard-term` must not depend on the clients or servers; each CLI maps
  its client's errors to `error::Failure`.
- Server-side unification (config, auth, error bodies, windows) is separate
  work (`mallard-common`, `mallard-duck`).
