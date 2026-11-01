#!/usr/bin/env bash
# Steady-state ingest at increasing rates against a resource-limited container.
# Usage: steady-state.sh [cpus] [memory] [duckdb_memory_limit] [rates...]
set -euo pipefail
CPUS=${1:-2}; MEM=${2:-2g}; DMEM=${3:-1200MB}; shift 3 || true
RATES=${*:-"100 500 1500 3000"}
IMG=${MALLARD_IMAGE:-mallard:dev}   # docker build -f deploy/docker/Dockerfile -t mallard:dev .
LOADGEN=${LOADGEN:-odl-loadgen}
docker rm -f mallard-bench >/dev/null 2>&1 || true
docker volume rm mallard-bench >/dev/null 2>&1 || true
docker run -d --name mallard-bench --cpus="$CPUS" --memory="$MEM" -v mallard-bench:/data \
  -p 24318:4318 -p 28080:8080 -e 'MALLARD_SERVER__API_TOKENS=["bench"]' \
  -e MALLARD_DUCKDB__MEMORY_LIMIT="$DMEM" -e MALLARD_OTEL__MAINTENANCE__COMPACT_INTERVAL=2m "$IMG" >/dev/null
sleep 3
for r in $RATES; do
  "$LOADGEN" --endpoint http://127.0.0.1:24318 --rate "$r" --duration 60s --batch 20 --concurrency 8 > /tmp/mallard-bench-lg.txt 2>&1 &
  LG=$!
  cpu=(); mem=()
  for _ in $(seq 1 11); do
    sleep 5
    read -r c m < <(docker stats --no-stream --format '{{.CPUPerc}} {{.MemUsage}}' mallard-bench | awk '{gsub("%","",$1); print $1, $2}')
    cpu+=("$c"); mem+=("$m")
  done
  wait $LG || true
  res=$(grep "done in" /tmp/mallard-bench-lg.txt)
  spans=$(echo "$res" | sed -E 's/.*avg ([0-9]+) spans\/s.*/\1/')
  thr=$(echo "$res" | sed -E 's/.* ([0-9]+) throttled.*/\1/')
  failed=$(echo "$res" | sed -E 's/.* ([0-9]+) failed.*/\1/')
  avgcpu=$(printf '%s\n' "${cpu[@]}" | awk '{s+=$1} END {printf "%.0f", s/NR}')
  maxcpu=$(printf '%s\n' "${cpu[@]}" | sort -n | tail -1)
  maxmem=$(printf '%s\n' "${mem[@]}" | sort -h | tail -1)
  printf "rate %5s traces/s  spans/s %7s  cpu avg %4s%% max %6s%%  mem max %9s  throttled %s failed %s\n" \
    "$r" "$spans" "$avgcpu" "$maxcpu" "$maxmem" "$thr" "$failed"
done
docker exec mallard-bench mallard --token bench telemetry status | sed -n '/Storage/,$p'
