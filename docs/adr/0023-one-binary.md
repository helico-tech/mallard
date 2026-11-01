# 0023 — One binary, with a slim build

## Status
accepted (supersedes the binary split of 0007; its crate split stays)

## Context
ADR 0007 split the product into `mallard-server` and `mallard`, because the CLI
is used far more widely than the server and should not carry DuckDB. That
worked, but it means two names to learn and two things to install: running
mallard locally needs both, and the image ships both. We want one thing to
install. Anyone who needs no DuckDB can still have that, as a slim build of the
same binary.

The size was the main cost of going back. Measured on 2026-11-01, Apple Silicon
(arm64), release profile (thin LTO, symbols stripped):

| build | size | release build | `mallard --version` |
|---|---:|---:|---:|
| default (with the server) | 81.9 MB | 3 min 46 s, with DuckDB's C++ | 11.6 ms |
| `--no-default-features` | 10.2 MB | 31 s | 4.5 ms |

Build times are from an empty target directory on a busy 18-core machine. Start-up
is the median of 80 runs. The 7 ms difference is what a CLI command pays for having
DuckDB linked in; it does not open DuckDB. In the Docker image (Linux arm64) the
default binary is 82.3 MB.

## Decision
- `crates/mallard` builds the only product binary. Feature `server`, on by
  default, adds `mallard serve`, `check [--connect]`, `maintenance [--full]` and
  `install-extensions`, from the `mallard-server` library crate.
- `mallard-server` has no binary. Its `commands` module is what those commands
  run, and the only place that installs logging and the Prometheus recorder, so
  CLI commands never get them. Argument parsing and printing are in
  `crates/mallard/src/server.rs`.
- `--no-default-features` builds the same CLI and TUI with no DuckDB in
  `cargo tree`, including dev-dependencies. CI checks the tree, clippy and the
  tests of that build. It is what `cargo install` users pick if they need only a
  client: much smaller, and quick to compile.
- The server commands are defined in both builds and hidden in the slim one, so
  `mallard serve` there is a usage error (exit 2) that says the build has no
  server, not "unknown command".
- `serve` is explicit. Plain `mallard` keeps its behaviour (usage, exit 2).
- `-c/--config` and `MALLARD_CONFIG` belong to the server commands and come after
  the command (`mallard serve -c file`). They are not global flags: they mean
  nothing to the CLI. `mallard -c file serve`, which the old binary accepted, is a
  usage error.
- The server commands follow ADR 0008. A config that does not load is exit 2,
  errors are one line (JSON with `--json`), data is on stdout. To keep that true
  for `maintenance`, all logs now go to stderr, also for `serve` (they were on
  stdout).
- Tests that run the binary live in `crates/mallard/tests`, use
  `env!("CARGO_BIN_EXE_mallard")`, and have `required-features = ["server"]`. They
  use `mallard-server` through the optional normal dependency. It must not be a
  dev-dependency: those cannot be optional, so DuckDB would be back in the slim
  tree and in its `cargo test`.

## Considered
- Keep two binaries (ADR 0007): the smallest CLI by default, but two things to
  install and two names.
- `mallard serve` starts a second binary: one name, but still two things to install
  and they can get out of step.

## Consequences
- One thing to install; `docker exec … mallard status` is the binary that serves.
- The default `cargo install --path crates/mallard` now compiles DuckDB and takes
  minutes; the default binary is about 8 times the size of the slim one. Anyone
  who minds uses `--no-default-features`.
- `cargo test -p mallard` builds DuckDB; `--no-default-features` does not.
- Breaking for anyone on the old names: the `mallard-server` command is gone,
  anything that relied on it serving without a subcommand must say `serve` (the
  image's default command does), `MALLARD_SERVER_BIN` is now `MALLARD_BIN`, and a
  log collector that reads only stdout must read stderr too.
- The crate split of ADR 0007 is what makes the slim build possible, so it stays.
- `target/debug/mallard` is whichever build ran last, so run `cargo build -p mallard`
  after a slim build before using that path (the e2e harness does not rebuild).
