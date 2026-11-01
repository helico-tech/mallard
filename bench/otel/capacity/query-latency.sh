#!/usr/bin/env bash
# Median latency (5 runs) of typical API calls for several time windows.
# Usage: query-latency.sh <api base url> <data end RFC3339> <trace id>
set -euo pipefail
API="$1/api/v1"; END="$2"; TID="$3"
med() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
t() { # url
  for _ in 1 2 3 4 5; do
    curl -s -o /dev/null -w '%{time_total}\n' -H "Authorization: Bearer ${ODL_TOKEN:-}" "$1"
  done | med | awk '{printf "%7.0f ms", $1*1000}'
}
printf "%-44s %10s %10s %10s %10s\n" "endpoint" "15m" "1h" "24h" "7d"
row() { # name path-with-params (window appended)
  printf "%-44s" "$1"
  for w in 15m 1h 24h 7d; do printf " %10s" "$(t "$API/$2&to=$END&since=$w")"; done; echo
}
row "services (RED)"                    "services?x=1"
row "operations"                        "operations?x=1"
row "spans/series (1 service)"          "spans/series?service=checkout"
row "traces search (service+errors)"    "traces?service=payment&errors_only=true&limit=50"
row "traces search (min duration)"      "traces?min_duration_ms=500&limit=50"
row "traces search (attribute)"         "traces?attributes=http.route%3D/checkout&limit=50"
row "trace by id"                       "traces/$TID?x=1"
row "logs search (text)"                "logs?q=timeout&limit=100"
row "logs (errors, 1 service)"          "logs?service=payment&min_severity=17&limit=100"
row "logs/series"                       "logs/series?x=1"
row "metrics list"                      "metrics?x=1"
row "metric p95 by service (histogram)" "metrics/query?name=app.request.duration&agg=p95&group_by=service.name"
row "metric rate (counter)"             "metrics/query?name=app.requests&agg=rate"
