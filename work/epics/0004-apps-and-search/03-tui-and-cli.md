---
status: todo
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

## Progress log

## Follow-ups
