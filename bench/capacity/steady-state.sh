#!/usr/bin/env bash
# Steady-state ingest at increasing rates against a resource-limited container.
# Usage: steady-state.sh [cpus] [memory] [duckdb_memory_limit] [rates...]
set -euo pipefail
CPUS=${1:-2}; MEM=${2:-2g}; DMEM=${3:-1200MB}; shift 3 || true
RATES=${*:-"100 500 1500 3000"}
IMG=${ODL_IMAGE:-otel-ducklake:dev}
LOADGEN=${LOADGEN:-odl-loadgen}
docker rm -f odl-bench >/dev/null 2>&1 || true
docker volume rm odl-bench >/dev/null 2>&1 || true
docker run -d --name odl-bench --cpus="$CPUS" --memory="$MEM" -v odl-bench:/data \
  -p 24318:4318 -p 28080:8080 \
  -e ODL_DUCKDB__MEMORY_LIMIT="$DMEM" -e ODL_MAINTENANCE__COMPACT_INTERVAL=2m "$IMG" >/dev/null
sleep 3
for r in $RATES; do
  "$LOADGEN" --endpoint http://127.0.0.1:24318 --rate "$r" --duration 60s --batch 20 --concurrency 8 > /tmp/odl-bench-lg.txt 2>&1 &
  LG=$!
  cpu=(); mem=()
  for _ in $(seq 1 11); do
    sleep 5
    read -r c m < <(docker stats --no-stream --format '{{.CPUPerc}} {{.MemUsage}}' odl-bench | awk '{gsub("%","",$1); print $1, $2}')
    cpu+=("$c"); mem+=("$m")
  done
  wait $LG || true
  res=$(grep "done in" /tmp/odl-bench-lg.txt)
  spans=$(echo "$res" | sed -E 's/.*avg ([0-9]+) spans\/s.*/\1/')
  thr=$(echo "$res" | sed -E 's/.* ([0-9]+) throttled.*/\1/')
  failed=$(echo "$res" | sed -E 's/.* ([0-9]+) failed.*/\1/')
  avgcpu=$(printf '%s\n' "${cpu[@]}" | awk '{s+=$1} END {printf "%.0f", s/NR}')
  maxcpu=$(printf '%s\n' "${cpu[@]}" | sort -n | tail -1)
  maxmem=$(printf '%s\n' "${mem[@]}" | sort -h | tail -1)
  printf "rate %5s traces/s  spans/s %7s  cpu avg %4s%% max %6s%%  mem max %9s  throttled %s failed %s\n" \
    "$r" "$spans" "$avgcpu" "$maxcpu" "$maxmem" "$thr" "$failed"
done
docker exec odl-bench odl status | sed -n '/Storage/,$p'
