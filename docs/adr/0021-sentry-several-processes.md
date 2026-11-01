# 0021 — Several Sentry processes on one catalog

## Status
accepted (amends `docs/sentry/adr/0001`: "one process per catalog")

## Context
With two pods on one Postgres catalog, a project created on pod A was unknown
on pod B, both pods could insert the same issue, regressions were found per
pod, project ids collided and every pod ran all maintenance (issue
`work/sentry/issues/0001`).

The tables are append-only and DuckLake has no unique keys or sequences, and
two concurrent `INSERT`s never conflict. What it does have: a transaction that
removes rows conflicts with every concurrent write to that table (measured,
ADR 0014). A second commit then fails, on any catalog, across processes.

Options for "maintenance runs in one process":
- A config flag: simple, but all pods of one Deployment share their config, so
  the chart would need a second Deployment for one special pod.
- A Postgres advisory lock: DuckDB's Postgres connections are pooled, so a
  session lock is held by whichever connection took it; it needs a separate
  client, and it only covers Postgres.
- A lease row in `meta`, taken by a transaction that deletes and inserts it: no
  new dependency, works on every catalog, needs no per-pod configuration.
- No election, as the otel side does (jitter): safe, but every pod does the
  work of every run, and they fail each other's runs.

## Decision
- **Lease.** Each flush interval a process tries the `maintenance_lease` row in
  `meta`: free, expired or its own → replace it (valid for 3 flush intervals).
  The holder runs timed flushes, retention and compaction. A process past
  `flush_rows` of its own inlined rows flushes anyway, because only it knows
  that count. The lease saves work; **safety does not depend on it**: overlapping
  maintenance rolls back in a conflict. `POST /v1/maintenance` ignores the lease.
- **Project ids** still count up, so DSNs stay short. `create_project` checks the
  slug, takes `max(id) + 1` and replaces the `project_seq` row in `meta`, all in
  one transaction. Concurrent calls conflict; the loser tries again and sees the
  other's id and slug. So slugs and ids are unique across processes.
- **Project cache.** A miss reads the projects again, at most once a second per
  process: ingest is open to anyone, and a made-up id must not cost a query.
- **Sync.** Every 30 s the writer thread reads all projects, and bundles and
  status changes of the last 5 minutes (other processes write with their own
  clock). All status cache changes stay on that thread, so a sync never replaces
  a newer status with an older one.
- **Issues.** A cache miss asks the lake (Sentry issue 0002), so a group another
  process created gets no new row, except when both create it within one batch.
  Then two `issues` rows exist, so **every read of `issues` must take one row per
  group** (`arg_min(..., first_seen)`).
- **`set_status`** reads the current status from the lake, not the cache: an
  API call is rare, and "already resolved" must be true.
- Startup tries again on a conflict: pods starting at once race on `CREATE`.

## Consequences
- More pods work on a Postgres catalog. Rows of `meta` named in
  `writer::ensure_meta_rows` are replaced, not appended: a small exception to the
  append-only rule, for coordination only.
- Other pods see a status change or a bundle after up to 30 s. In that time an
  event on another pod does not reopen a just-resolved issue, and symbolication
  may skip a just-uploaded bundle.
- Per process, on purpose: the limit on new issues (N pods allow N × 1000 per
  hour), the event id dedupe (an SDK retry through another pod is stored twice)
  and the sourcemap cache.
- Clocks must agree within 5 minutes, or a sync misses a status change until a
  cache miss reads it.
- The lease writes one small snapshot per flush interval, also on an idle
  system.
- A race for the same new group on two pods can still write two rows; raw
  `POST /v1/sql` queries over `issues` see both.
