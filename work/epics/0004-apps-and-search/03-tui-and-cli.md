---
status: done
created: 2026-11-01
---
# 03 — TUI and CLI

## Goal
The TUI gets the same app scope and suggestions as the web UI, and the CLI can
say `--app`.

## Acceptance criteria
- `--app <slug>` on commands that take `-p` or `-S`. Old flags behave as before. No
  `app=` in the filter language (ADR 0028).
- One app picker in `mallard tui`; it scopes F1 and F2. "All apps" is the default:
  telemetry shows everything, and the errors tab asks for an app.
- One time window across F1 and F2 (story 04 has the rule).
- The filter bar shows suggestions; Tab accepts. Typing never waits on the network.
- Text in suggestions passes `mallard_term` cleaning, like all untrusted text.
- `docs/cli.md` and `docs/agents.md` updated; agent CLI tests cover `--app`.

## Plan
After stories 1 and 2. Put the suggestion list in `mallard-term`, so both tabs use
one widget. Fetch in the background and cache per window.

Steps (2026-11-01):
1. Client: `service` on the telemetry query structs becomes a list that the client
   sends as repeated parameters (left out of serialization, the `tag` pattern). The
   OpenAPI document stays the same.
2. CLI: global `--app <slug>`, worked out in one new file `crates/mallard/src/scope.rs`.
   It reads `/api/v1/apps` once, before the command runs. Rule: `--app` replaces `-p` and
   `-S`, so giving either next to it on the command line is exit 2; `MALLARD_PROJECT` from
   the environment gives way to the flag. A missing side or unknown app is exit 4; if the
   list had warnings it is exit 6, because the side may only be unreadable. A command
   with neither `-p` nor `-S` use is exit 2.
3. `mallard-term`: `tui::suggest` (the widget, no I/O: edits go in, what to fetch comes out,
   debounce, cache per scope, cleaning at the door), `tui::Scope` and the window presets.
4. Telemetry pane: `set_scope`, `set_hosted`; scoped queries; empty state; suggestions in
   the trace and log filters. Errors pane: the same, `key=value` in `/` become tag filters
   (else tag suggestions have nothing to filter), `d` jump to 24h on an empty list.
5. Host (`crates/mallard/src/tui.rs`): the picker (`p`), one window (`t`/`T`), the bar.
   Panes in a host return `Ignored` for `t`, `T`, `p`, so all the logic sits in the host.
6. Docs, agent CLI tests, a real run in a pseudo-terminal, screenshots if they changed.

## Progress log
- 2026-11-01: Built as planned. Decisions the criteria left open:
  - `--app` with `-p` or `-S` on the command line is always exit 2, also when they would agree.
    `MALLARD_PROJECT` gives way to the flag: a default must not make an explicit flag fail.
  - An app's services are read for the command's window plus the last 24h, so a quiet app does
    not look unknown under `--since 5m`. The list is read once, so `--wait` does not wait for an
    app's first data: exit 4, and the hint says to use `-S`.
  - Missing from a list with warnings is exit 6, not 4 (as `find`: a side that failed never turns
    into "not found"). `--app` where it cannot apply, and `events list --trace`, are exit 2.
  - `service` on the query structs is now a list that serialization leaves out, the `tag` pattern.
    Not with `skip_serializing` as planned: the OpenAPI derive then drops the parameter (the
    test caught it), so `skip_serializing_if`. The documents stay the same.
  - In `mallard tui` the tabs return `t`, `T` and `p` as `Ignored` and the host acts on them, so one
    place holds the window and the app. The tabs on their own keep their old keys and windows.
  - The issue filter reads `key=value` as a tag, the other words as text. The criteria only ask
    for tag suggestions, which would have nothing to filter on otherwise. A text search for `a=b`
    is now a tag filter, as in the web UI (a message from the web side, same day).
  - `d` jumps to 24h on an empty issue list. Asks the host through `Outcome::SetWindow`.
  - Opening an event of another project (from `x`) moves the picker along, so that it never
    disagrees with what is shown, also on the telemetry tab.
- 2026-11-01: The web side finished first, and its rules were adopted here: empty `--app` is no app;
  `p` is in `keys::SHARED` (no clash in `mallard tui`; the log detail popup, which takes every key, lets it
  through; the tabs on their own leave it out of their help); `op=` values read at most 24h;
  "switched to X" on a project change; `docs/web/parity.md` rows updated. Knowing differences:
  a `service=` outside the app is refused with a message and the old filter stays, as every bad filter
  in the TUI (nothing is read for it; the web UI empties the view). `-p` is the picker's first choice,
  and `-p` next to `--app` is exit 2, where the web UI lets a project in the URL win. Tab takes the
  first suggestion without moving to it (the web UI waits until you move; parity.md says intended).
- Real run: `mallard serve` with `scripts/demo-seed.py`, the real binary in a pseudo-terminal, frames
  read through a terminal emulator: All apps, the picker, an app without a project and one without
  telemetry, `t` on both tabs, tag and `op=` and `level=` suggestions, Tab, `d`, `service=` outside
  the app, `trace=` across apps, help. CLI: `--app` on logs and issues, a missing side (exit 4), `-p`
  next to it (exit 2). No fault in what was looked at.
- Screenshots regenerated: the top bar has the app and the window, and errors start at 1h.

## Follow-ups
- [issue 0146](../../issues/0146-telemetry-tui-draws-uncleaned-text.md): the telemetry tab draws
  some server text without cleaning it (bidi characters reach the terminal).
- [issue 0147](../../issues/0147-project-flag-ignored-without-a-project.md): `-p` is silently
  ignored where it means nothing, where `--app` is exit 2.
- [issue 0143](../../issues/0143-find-cli-hides-warnings.md) is fixed: `mallard find` prints warnings.
