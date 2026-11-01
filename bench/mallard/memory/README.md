# Memory of both sides in one container

Runs `mallard serve` in a Docker container with a hard memory limit, with both load
generators, a little query load, and maintenance on short intervals. An out-of-memory
kill is a result. The results are in
[docs/otel/capacity.md](../../../docs/otel/capacity.md#both-sides-in-one-container).

```sh
docker build -f deploy/docker/Dockerfile -t mallard:dev .
cargo build --release -p mallard-sentry-loadgen -p mallard-otel-loadgen

# name, container memory, duckdb.memory_limit, otel.ingest.max_buffer_bytes, duckdb.threads, duration
SENTRY_RATE=300 OTEL_RATE=250 bench/mallard/memory/run.sh medium 2g 1GB 128MB 4 25m
SENTRY_RATE=20 SENTRY_CONC=64 OTEL_RATE=50 OTEL_BATCH=10 bench/mallard/memory/run.sh light 2g 1GB 128MB 4 20m
bench/mallard/memory/run.sh heavy 4g 2GB 256MB 8 15m              # 1000 events/s, 11k spans/s
SENTRY_RATE=0 OTEL_RATE=10000 bench/mallard/memory/run.sh sat 2g 1GB 128MB 4 10m   # all it accepts

# One Sentry compaction of a backlog: name, container memory, limit, threads, events
RATE=1500 ROUNDS=3 bench/mallard/memory/compaction.sh backlog 4g 2GB 8 1000000
```

Results go to `bench-out/<name>/`, and `summary.sh` prints them at the end of a run:

- `samples.txt`: each second: time, container memory, container peak, anon, file cache,
  process RSS, process peak RSS, threads.
- `server.log`, `metrics.txt`, `otel-stats.json`, `sentry-loadgen.txt`, `otel-loadgen.txt`,
  `queries.txt`.

## What to read in the results

- Use the process peak RSS (`VmHWM`) and anon memory. Container peak also counts file cache,
  which the kernel reclaims, so it reaches the limit in nearly every run.
- A run counts as passed only with no kill, no `Out of Memory` line, and all compactions
  done. Surviving is not enough: a failed Sentry compaction also fails the writer's batch.
- `--cpus=N` with `threads = 0` gives N DuckDB threads. `--cpuset-cpus` does not: DuckDB
  still sees every CPU of the machine.
- Other settings are environment variables at the top of `run.sh`. To run against compose,
  start the stack and pass `ATTACH=<container>` and `PORT=<its http port>`.
- Never start a run on a port an earlier run's load generators may still use, and do not edit
  `run.sh` while it runs: bash reads the file as it goes.
