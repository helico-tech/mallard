---
status: todo
type: bug
created: 2026-11-02
---
# `mallard projects show <p>` always fails with a usage error

## Problem
Since issue 0147 the argument of `projects show` is taken for the global `-p` flag, so the command never runs:

```
error: -p does not apply to this command; it works on issues, events, releases, sourcemaps, find, series errors and tui. For telemetry use -S or --app
```

Exit 2, for any project, without asking the server. The `--help` example and ADR 0008 list the command.

## How to reproduce
```sh
mallard projects show web
```

## Notes
Found while fixing issue 0151, which did not change it. Probably the positional `project` of `ProjectsCmd::Show`
has the same clap id as the global flag, and `crates/mallard/src/scope.rs` asks whether that id came from the
command line. Rename the id, and add `projects show` to a test: `project_flag_only_where_there_is_a_project` tests
only that `-p` is refused.
