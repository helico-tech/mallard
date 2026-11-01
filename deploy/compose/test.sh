#!/usr/bin/env bash
# Run the compose stack and check it end to end.
#
#   deploy/compose/test.sh <image:tag>
#
# The image must already exist locally. Two scenarios, each brought up, checked,
# restarted, checked again and removed with its volumes:
#   postgres  RustFS (S3) + Postgres catalog: the stack's default
#   duckdb    RustFS (S3) + DuckDB-file catalog on the mallard-data volume
# Both have an ingest token on and a read-only root filesystem, set by an override
# file, so the compose file stays what users run.
# Checks: the server gets ready, OTLP without the token is refused, a Sentry event and an
# OTLP/HTTP span with the same trace id end up in `mallard trace`, OTLP/gRPC through
# the compose network (telemetrygen), /metrics, `mallard status` in the container. Then
# the container is replaced and the same data must still be there.
#
# It uses its own compose project and ports, so it runs next to a stack you already
# have. The credentials are the compose file's local-only defaults.
# Needs: docker compose, curl, jq. Used by CI (.github/workflows/ci.yml).
set -Eeuo pipefail
cd "$(dirname "$0")/../.."

image=${1:?usage: $0 <image:tag>}
project=${MALLARD_TEST_PROJECT:-mallard-compose-test}
port=${MALLARD_TEST_PORT:-18080}
token=test-token
ingest=ingest-token
telemetrygen=ghcr.io/open-telemetry/opentelemetry-collector-contrib/telemetrygen:v0.111.0
url=http://localhost:$port

export MALLARD_IMAGE=$image MALLARD_API_TOKEN=$token MALLARD_PORT=$port
export MALLARD_OTLP_HTTP_PORT=${MALLARD_TEST_OTLP_HTTP_PORT:-14318}
export MALLARD_OTLP_GRPC_PORT=${MALLARD_TEST_OTLP_GRPC_PORT:-14317}
export MALLARD_WEB_PORT=${MALLARD_TEST_WEB_PORT:-18088}

override=$(mktemp)
cat >"$override" <<EOF
services:
  mallard:
    read_only: true
    tmpfs: [/tmp]
    environment:
      MALLARD_SERVER__INGEST_TOKENS: $ingest
      MALLARD_OTEL__INGEST__FLUSH_INTERVAL: 1s
EOF

compose() { docker compose -p "$project" -f deploy/compose/docker-compose.yml -f "$override" "$@"; }
log() { printf '\n== %s\n' "$*"; }
down() { compose down -v --remove-orphans --timeout 60 >/dev/null 2>&1 || true; }
cleanup() { down; rm -f "$override"; }
dump() {
  compose ps -a || true
  compose logs --tail 200 || true
}
trap cleanup EXIT
trap 'echo "failed at line $LINENO" >&2; dump' ERR

# Retry "$@" every 2 s for up to $tries times.
retry() {
  local tries=$1; shift
  for _ in $(seq 1 "$tries"); do "$@" && return 0; sleep 2; done
  "$@"
}

ready() { curl -fsS "$url/readyz" >/dev/null; }
in_container() { compose exec -T mallard mallard --url http://localhost:8080 --token "$token" "$@"; }
hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }

# The trace has its span and its error, the gRPC spans and the project are there.
has_trace() {
  in_container --format json trace "$1" | jq -e '(.spans | length) >= 1 and (.errors | length) >= 1' >/dev/null
}
has_grpc_spans() {
  curl -fsS -H "Authorization: Bearer $token" "$url/otel/api/v1/traces?service=compose-grpc&since=1h" |
    jq -e 'length > 0' >/dev/null
}
has_project() {
  curl -fsS -H "Authorization: Bearer $token" "$url/sentry/v1/projects" | jq -e 'any(.[]; .slug == "compose")' >/dev/null
}

scenario() {
  local name=$1 trace span p id key now status
  log "$name: up"
  down
  compose up -d --no-build
  retry 60 ready

  trace=$(hex 16)
  span=$(hex 8)
  now=$(date +%s)

  log "$name: sentry project + event in trace $trace"
  p=$(curl -fsS -XPOST -H "Authorization: Bearer $token" -H 'content-type: application/json' \
    "$url/sentry/v1/projects" -d '{"name":"compose"}')
  id=$(jq -r .id <<<"$p")
  key=$(jq -r .public_key <<<"$p")
  printf '%s\n%s\n%s\n' \
    "{\"event_id\":\"$(hex 16)\"}" \
    '{"type":"event"}' \
    "{\"timestamp\":$now,\"platform\":\"python\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"ComposeError\",\"value\":\"from compose\"}]},\"contexts\":{\"trace\":{\"trace_id\":\"$trace\",\"span_id\":\"$span\"}}}" |
    curl -fsS -XPOST -H "x-sentry-auth: Sentry sentry_version=7, sentry_key=$key" \
      -H 'content-type: application/x-sentry-envelope' --data-binary @- \
      "$url/sentry/api/$id/envelope/" >/dev/null

  local spans="{\"resourceSpans\":[{
    \"resource\":{\"attributes\":[{\"key\":\"service.name\",\"value\":{\"stringValue\":\"compose-http\"}}]},
    \"scopeSpans\":[{\"spans\":[{\"traceId\":\"$trace\",\"spanId\":\"$span\",\"name\":\"GET /compose\",\"kind\":2,
      \"startTimeUnixNano\":\"${now}000000000\",\"endTimeUnixNano\":\"${now}050000000\",\"status\":{\"code\":2}}]}]}]}"

  log "$name: OTLP/HTTP needs the ingest token"
  status=$(curl -sS -o /dev/null -w '%{http_code}' -XPOST -H 'content-type: application/json' \
    "$url/v1/traces" -d "$spans")
  [ "$status" = 401 ] || { echo "expected 401 without a token, got $status" >&2; false; }
  curl -fsS -XPOST -H "Authorization: Bearer $ingest" -H 'content-type: application/json' \
    "http://localhost:$MALLARD_OTLP_HTTP_PORT/v1/traces" -d "$spans" >/dev/null

  log "$name: OTLP/gRPC (telemetrygen)"
  docker run --rm --network "${project}_default" "$telemetrygen" \
    traces --otlp-endpoint mallard:4317 --otlp-insecure --traces 3 --service compose-grpc \
    --otlp-header "Authorization=\"Bearer $ingest\""

  log "$name: read it back"
  retry 30 has_trace "$trace"
  retry 30 has_grpc_spans
  curl -fsS "$url/metrics" | grep '^# TYPE' >/dev/null
  in_container status

  log "$name: replace the container; the data stays"
  compose stop mallard
  compose up -d --no-build --no-deps --force-recreate mallard
  retry 60 ready
  retry 15 has_trace "$trace"
  retry 15 has_grpc_spans
  has_project
  down
}

unset MALLARD_CATALOG
scenario postgres
export MALLARD_CATALOG=/data/catalog.ducklake
scenario duckdb
log ok
