#!/usr/bin/env bash
# Both sides under load in one memory-limited container, with maintenance on
# short intervals. An out-of-memory kill is a result, not an error of this script.
# Usage: run.sh <name> <container memory> <duckdb.memory_limit> <max_buffer_bytes> <duckdb.threads> <duration>
#   e.g. run.sh b 2g 768MB 256MB 0 15m      (see README.md for all settings)
set -uo pipefail
NAME=$1; MEM=$2; LIMIT=$3; BUFFER=$4; THREADS=$5; DURATION=$6

IMG=${MALLARD_IMAGE:-mallard:dev}                # docker build -f deploy/docker/Dockerfile -t mallard:dev .
BIN=${BIN:-target/release}                       # the two load generators (they run on the host)
CPUS=${CPUS:-8}                                  # threads=0 gives one DuckDB worker per CPU of this quota (not with --cpuset-cpus)
PORT=${PORT:-28180}                              # PORT is http, PORT+1 is OTLP/HTTP
SENTRY_RATE=${SENTRY_RATE:-1000}                 # events/s, 0 = as fast as it accepts
SENTRY_CONC=${SENTRY_CONC:-1024}                 # each waits for its commit, so fewer clients cap the rate
OTEL_RATE=${OTEL_RATE:-1000}                     # traces/s, about 11 spans per trace
OTEL_BATCH=${OTEL_BATCH:-20}
OTEL_CONC=${OTEL_CONC:-8}
SENTRY_COMPACT=${SENTRY_COMPACT:-2m}             # default 1h
SENTRY_FLUSH=${SENTRY_FLUSH:-30s}                # default 5m
OTEL_COMPACT=${OTEL_COMPACT:-2m}                 # default 10m
OTEL_FULL=${OTEL_FULL:-5m}                       # default 6h
OTEL_RETENTION=${OTEL_RETENTION:-5m}             # default 1h: snapshot expiry and file cleanup
SETTLE=${SETTLE:-60}                             # seconds after the load, so a last compaction runs
OUT=${OUT:-bench-out}/$NAME
QUERIES=${QUERIES:-1}                            # 0: no query load
EXTRA_ENV=${EXTRA_ENV:-}                          # more -e settings for the container, e.g. "-e MALLOC_ARENA_MAX=2"
SIDES=${SIDES:-both}                              # both | sentry | otel
TOKEN=${TOKEN:-bench}
ATTACH=${ATTACH:-}                                # run against this running container (compose) instead of starting one
C=${ATTACH:-memdefaults-$NAME}
HTTP=http://127.0.0.1:$PORT
OTLP=http://127.0.0.1:$((PORT + 1))
mkdir -p "$OUT"

if [ -z "$ATTACH" ]; then
docker rm -f "$C" >/dev/null 2>&1 || true
docker volume rm "$C" >/dev/null 2>&1 || true
docker run -d --name "$C" --memory="$MEM" --memory-swap="$MEM" --cpus="$CPUS" -v "$C:/data" \
  -p "127.0.0.1:$PORT:8080" -p "127.0.0.1:$((PORT + 1)):4318" \
  -e MALLARD_SERVER__API_TOKENS="$TOKEN" \
  -e MALLARD_DUCKDB__MEMORY_LIMIT="$LIMIT" -e MALLARD_DUCKDB__THREADS="$THREADS" \
  -e MALLARD_OTEL__INGEST__MAX_BUFFER_BYTES="$BUFFER" \
  -e MALLARD_SENTRY__MAINTENANCE__COMPACT_INTERVAL="$SENTRY_COMPACT" \
  -e MALLARD_SENTRY__MAINTENANCE__FLUSH_INTERVAL="$SENTRY_FLUSH" \
  -e MALLARD_OTEL__MAINTENANCE__COMPACT_INTERVAL="$OTEL_COMPACT" \
  -e MALLARD_OTEL__MAINTENANCE__FULL_COMPACT_INTERVAL="$OTEL_FULL" \
  -e MALLARD_OTEL__MAINTENANCE__RETENTION_INTERVAL="$OTEL_RETENTION" \
  $EXTRA_ENV "$IMG" serve >/dev/null || exit 1
fi
for _ in $(seq 1 60); do curl -fs "$HTTP/healthz" >/dev/null && break; sleep 1; done

# One long-running sampler in the container. High-water marks: memory.peak and VmHWM.
docker exec "$C" bash -c 'while :; do
    a=$(grep -m1 "^anon " /sys/fs/cgroup/memory.stat); f=$(grep -m1 "^file " /sys/fs/cgroup/memory.stat)
    r=$(grep -m1 VmRSS /proc/1/status); h=$(grep -m1 VmHWM /proc/1/status)
    echo "$(date -u +%H:%M:%S) $(cat /sys/fs/cgroup/memory.current) $(cat /sys/fs/cgroup/memory.peak) ${a#anon } ${f#file } ${r//[^0-9]/} ${h//[^0-9]/} $(ls /proc/1/task | wc -l)"
    sleep 1; done' > "$OUT/samples.txt" 2>/dev/null &
SAMPLER=$!

[ "$SIDES" != otel ] && "$BIN/mallard-sentry-loadgen" --url "$HTTP" --token "$TOKEN" ingest --rate "$SENTRY_RATE" --duration "$DURATION" \
  --concurrency "$SENTRY_CONC" > "$OUT/sentry-loadgen.txt" 2>&1 &
LG1=$!
[ "$SIDES" != sentry ] && "$BIN/mallard-otel-loadgen" --endpoint "$OTLP" --rate "$OTEL_RATE" --duration "$DURATION" --batch "$OTEL_BATCH" \
  --concurrency "$OTEL_CONC" > "$OUT/otel-loadgen.txt" 2>&1 &
LG2=$!

# A little query load too, as a real pod has: it shares DuckDB's memory.
( while [ "$QUERIES" != 0 ] && { kill -0 $LG1 2>/dev/null || kill -0 $LG2 2>/dev/null; }; do
    sleep 30
    "$BIN/mallard-sentry-loadgen" --url "$HTTP" --token "$TOKEN" query --reps 1 >> "$OUT/queries.txt" 2>&1 \
      || echo "sentry query FAILED $(date -u +%H:%M:%S)" >> "$OUT/queries.txt"
    for q in services 'traces?limit=20' 'logs?limit=20'; do
      s=$(curl -s -o /dev/null -w '%{http_code}' -m 60 -H "Authorization: Bearer $TOKEN" "$HTTP/otel/api/v1/$q")
      echo "otel $q $s $(date -u +%H:%M:%S)" >> "$OUT/queries.txt"
    done
  done ) &
QUERIES=$!

wait $LG1; wait $LG2; wait $QUERIES
sleep "$SETTLE"

curl -s -m 30 "$HTTP/metrics" > "$OUT/metrics.txt"
curl -s -m 30 -H "Authorization: Bearer $TOKEN" "$HTTP/otel/api/v1/stats" > "$OUT/otel-stats.json"
docker exec "$C" sh -c 'cat /sys/fs/cgroup/memory.peak /sys/fs/cgroup/memory.events; grep -E "VmHWM|Threads" /proc/1/status; du -sh /data/.tmp /data/data 2>/dev/null; find /data -name "*.parquet" | wc -l; find /data -path "*sentry/events*" -name "*.parquet" | wc -l' > "$OUT/final.txt" 2>&1
docker exec "$C" sh -c 'for t in /proc/1/task/*; do cat "$t/comm"; done | sort | uniq -c | sort -rn | head -20' > "$OUT/threads.txt" 2>&1
kill $SAMPLER 2>/dev/null
docker inspect -f 'status={{.State.Status}} oom_killed={{.State.OOMKilled}} exit={{.State.ExitCode}} restarts={{.RestartCount}}' "$C" > "$OUT/state.txt"
docker logs "$C" > "$OUT/server.log" 2>&1
"$(dirname "$0")/summary.sh" "$OUT"

if [ -z "$ATTACH" ] && [ "${KEEP:-0}" = 0 ]; then
  docker rm -f "$C" >/dev/null 2>&1
  docker volume rm "$C" >/dev/null 2>&1
fi
