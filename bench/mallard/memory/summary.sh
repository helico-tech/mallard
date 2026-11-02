#!/usr/bin/env bash
# One run's results, from the directory run.sh wrote.
# Sampled peak = the highest sample; the last two are the kernel's own high-water marks.
O=$1
metric() { awk -v m="$1" 'index($0, m) == 1 && !/^#/ {s += $NF} END {printf "%d", s}' "$O/metrics.txt" 2>/dev/null; }
gib() { awk -v b="$1" 'BEGIN {printf "%.2f", b / 1073741824}'; }
echo "== $O"
cat "$O/state.txt"
awk 'NF >= 8 {n++; if ($2 > c) c = $2; if ($3 > p) p = $3; if ($4 > a) a = $4; if ($6 > r) r = $6; if ($7 > h) h = $7; if ($8 > t) t = $8; if (NR == 1) s = $1; e = $1}
     END {printf "samples %d (%s to %s UTC)\ncontainer peak %.2f GiB (memory.peak), max current %.2f GiB, max anon %.2f GiB\nprocess peak RSS %.2f GiB (VmHWM), max sampled RSS %.2f GiB, threads %d\n",
          n, s, e, p / 2^30, c / 2^30, a / 2^30, h / 2^20, r / 2^20, t}' "$O/samples.txt"
grep -E '^(max|oom|oom_kill) ' "$O/final.txt" | tr '\n' ' '; echo
tail -4 "$O/final.txt" | tr '\n' ' '; echo
echo "duckdb out-of-memory lines: $(grep -ciE 'out of memory|failed to allocate|could not allocate' "$O/server.log")"
echo "error or warn lines: $(grep -cE ' (ERROR|WARN) ' "$O/server.log")"
echo "sentry compactions ok: $(grep -c 'compaction and snapshot expiry done' "$O/server.log"), failed: $(grep -c 'compaction failed' "$O/server.log")"
echo "otel maintenance ok: $(grep -c 'maintenance done' "$O/server.log"), failed: $(grep -c 'maintenance failed' "$O/server.log")"
echo "otel merges with files: $(grep 'maintenance done' "$O/server.log" | grep -vc 'merged 0 files\|task=.*retention' )"
if [ -s "$O/metrics.txt" ]; then
  echo "sentry: stored $(metric mallard_sentry_events_stored_total) rejected $(metric mallard_sentry_ingest_rejected_total) write errors $(metric mallard_sentry_write_errors_total) maintenance errors $(metric mallard_sentry_maintenance_errors_total)"
  echo "otel: items $(metric mallard_otel_ingest_items_total) throttled requests $(metric mallard_otel_ingest_throttled_requests_total) flush errors $(metric mallard_otel_flush_errors_total) dropped rows $(metric mallard_otel_dropped_rows_total) maintenance errors $(metric mallard_otel_maintenance_errors_total)"
else
  echo "no /metrics at the end: the server is gone"
fi
echo "loadgen sentry: $(tail -12 "$O/sentry-loadgen.txt" | tr -s ' ' | tr '\n' ';' | cut -c1-300)"
echo "loadgen otel: $(grep 'done in' "$O/otel-loadgen.txt" || tail -1 "$O/otel-loadgen.txt")"
echo "queries: $(grep -c ' 200 ' "$O/queries.txt" 2>/dev/null) otel ok; not ok: $(grep -cE 'FAILED|otel [^ ]+ [^2]' "$O/queries.txt" 2>/dev/null)"
