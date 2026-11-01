#!/usr/bin/env bash
# One Sentry compaction of a backlog (issue 0120): ingest <events> with periodic compaction
# out of the way, then POST /sentry/v1/maintenance and record the peak of that step alone.
# Usage: compaction.sh <name> <container memory> <duckdb.memory_limit> <duckdb.threads> <events>
#   e.g. compaction.sh t8 4g 2GB 8 1000000
set -uo pipefail
NAME=$1; MEM=$2; LIMIT=$3; THREADS=$4; EVENTS=$5
IMG=${MALLARD_IMAGE:-mallard:dev}
BIN=${BIN:-target/release}
CPUS=${CPUS:-8}
PORT=${PORT:-28280}
OTEL_RATE=${OTEL_RATE:-100}                      # traces/s next to it; a quiet second side
ROUNDS=${ROUNDS:-1}                              # maintenance calls in a row
RATE=${RATE:-0}                                  # events/s, 0 = as fast as it accepts (big batches skip the small files)
QUERY=${QUERY:-0}                                # 1: time the Sentry queries after compaction
OUT=${OUT:-bench-out}/$NAME
TOKEN=bench
C=memdefaults-$NAME
HTTP=http://127.0.0.1:$PORT
mkdir -p "$OUT"
docker rm -f "$C" >/dev/null 2>&1 || true
docker volume rm "$C" >/dev/null 2>&1 || true
docker run -d --name "$C" --memory="$MEM" --memory-swap="$MEM" --cpus="$CPUS" -v "$C:/data" \
  -p "127.0.0.1:$PORT:8080" -p "127.0.0.1:$((PORT + 1)):4318" \
  -e MALLARD_SERVER__API_TOKENS="$TOKEN" -e MALLARD_DUCKDB__MEMORY_LIMIT="$LIMIT" -e MALLARD_DUCKDB__THREADS="$THREADS" \
  -e MALLARD_SENTRY__MAINTENANCE__COMPACT_INTERVAL=24h -e MALLARD_OTEL__MAINTENANCE__COMPACT_INTERVAL=2m \
  "$IMG" serve >/dev/null || exit 1
for _ in $(seq 1 60); do curl -fs "$HTTP/healthz" >/dev/null && break; sleep 1; done
"$BIN/mallard-otel-loadgen" --endpoint "http://127.0.0.1:$((PORT + 1))" --rate "$OTEL_RATE" --duration 2h --batch 20 --concurrency 4 > "$OUT/otel-loadgen.txt" 2>&1 &
OTEL=$!
"$BIN/mallard-sentry-loadgen" --url "$HTTP" --token "$TOKEN" ingest --rate "$RATE" --events "$EVENTS" --duration 2h --concurrency 1024 > "$OUT/sentry-loadgen.txt" 2>&1
hwm() { docker exec "$C" sh -c 'grep -m1 VmHWM /proc/1/status | tr -dc 0-9' 2>/dev/null; }
echo "ingested: $(grep -E 'accepted|throughput|failed' "$OUT/sentry-loadgen.txt" | tr -s ' ' | tr '\n' ';')  peak RSS so far $(hwm) kB" | tee "$OUT/result.txt"
sleep 5
for i in $(seq 1 "$ROUNDS"); do
  docker exec "$C" sh -c 'echo 5 > /proc/1/clear_refs'   # restart the high-water mark
  t0=$(date +%s)
  code=$(curl -s -o "$OUT/maintenance.json" -w '%{http_code}' -m 900 -XPOST -H "Authorization: Bearer $TOKEN" "$HTTP/sentry/v1/maintenance")
  echo "maintenance $i: http $code in $(($(date +%s) - t0)) s, peak RSS in this step $(hwm) kB, answer $(head -c 300 "$OUT/maintenance.json")" | tee -a "$OUT/result.txt"
done
kill $OTEL 2>/dev/null
[ "$QUERY" = 0 ] || "$BIN/mallard-sentry-loadgen" --url "$HTTP" --token "$TOKEN" query --reps 5 | tee "$OUT/queries.txt"
docker exec "$C" sh -c 'cat /sys/fs/cgroup/memory.peak /sys/fs/cgroup/memory.events; du -sh /data/.tmp /data/data; find /data -name "*.parquet" -path "*sentry/events*" | wc -l' 2>&1 | tr '\n' ' ' | tee -a "$OUT/result.txt"; echo
docker inspect -f 'status={{.State.Status}} oom_killed={{.State.OOMKilled}} exit={{.State.ExitCode}}' "$C" | tee -a "$OUT/result.txt"
docker logs "$C" > "$OUT/server.log" 2>&1
echo "duckdb out-of-memory lines: $(grep -c 'Out of Memory' "$OUT/server.log"); compaction failed: $(grep -c 'compaction failed' "$OUT/server.log")" | tee -a "$OUT/result.txt"
docker rm -f "$C" >/dev/null 2>&1
docker volume rm "$C" >/dev/null 2>&1
