#!/usr/bin/env bash
# Install the Helm chart on a kind cluster and check it end to end.
#
#   deploy/helm/kind/test.sh <image:tag>
#
# The image must already be in the cluster (`kind load docker-image`).
# Two scenarios, each installed, checked and uninstalled:
#   duckdb  catalog file and Parquet files on the PVC; restart keeps the data
#   s3      RustFS (S3) + Postgres catalog from storage.yaml, ingest token on
# Checks: probes go ready under readOnlyRootFilesystem, a Sentry event and an
# OTLP/HTTP span with the same trace id end up in `mallard trace`, OTLP/gRPC
# through the Service (telemetrygen), /metrics, `mallard status` in the pod.
# Needs: kubectl, helm, curl, jq. Used by CI (.github/workflows/ci.yml).
set -Eeuo pipefail
cd "$(dirname "$0")/../../.."

image=${1:?usage: $0 <image:tag>}
chart=deploy/helm/mallard
here=deploy/helm/kind
token=test-token
telemetrygen=ghcr.io/open-telemetry/opentelemetry-collector-contrib/telemetrygen:v0.111.0
pf=
last_trace=

log() { printf '\n== %s\n' "$*"; }
cleanup() { if [ -n "$pf" ]; then kill "$pf" 2>/dev/null || true; wait "$pf" 2>/dev/null || true; pf=; fi; }
trap cleanup EXIT
trap 'echo "failed at line $LINENO" >&2; dump' ERR

# Retry "$@" every 2 s for up to $tries times.
retry() {
  local tries=$1; shift
  for _ in $(seq 1 "$tries"); do "$@" && return 0; sleep 2; done
  "$@"
}

dump() {
  kubectl get pods,pvc,svc -o wide || true
  kubectl describe pods -l app.kubernetes.io/instance=mallard || true
  kubectl logs -l app.kubernetes.io/instance=mallard --tail=200 --all-containers --prefix || true
}

install() {
  local name=$1; shift
  log "install: $name"
  helm install mallard "$chart" \
    --set image.repository="${image%:*}" --set image.tag="${image##*:}" --set image.pullPolicy=Never \
    --set resources.requests.cpu=100m --set resources.requests.memory=256Mi \
    --set logJson=false \
    --set-string extraConfig=$'[otel.ingest]\nflush_interval = "1s"' \
    "$@"
  kubectl rollout status deploy/mallard --timeout=5m
}

port_forward() {
  cleanup
  # Local ports apart from the usual ones, so nothing else on the host answers.
  kubectl port-forward svc/mallard 18080:80 19090:9090 &
  pf=$!
  retry 30 curl -fsS localhost:18080/readyz >/dev/null
}

# Errors and spans that share a trace id, over HTTP; spans over gRPC.
check() {
  local ingest=${1:-}
  local trace span auth=()
  trace=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
  span=$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')
  [ -n "$ingest" ] && auth=(-H "Authorization: Bearer $ingest")

  log "sentry project + event in trace $trace"
  local p id key now
  p=$(curl -fsS -XPOST -H "Authorization: Bearer $token" -H 'content-type: application/json' \
    localhost:18080/sentry/v1/projects -d '{"name":"kind"}')
  id=$(jq -r .id <<<"$p")
  key=$(jq -r .public_key <<<"$p")
  now=$(date +%s)
  printf '%s\n%s\n%s\n' \
    "{\"event_id\":\"$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')\"}" \
    '{"type":"event"}' \
    "{\"timestamp\":$now,\"platform\":\"python\",\"level\":\"error\",\"exception\":{\"values\":[{\"type\":\"KindError\",\"value\":\"from kind\"}]},\"contexts\":{\"trace\":{\"trace_id\":\"$trace\",\"span_id\":\"$span\"}}}" |
    curl -fsS -XPOST -H "x-sentry-auth: Sentry sentry_version=7, sentry_key=$key" \
      -H 'content-type: application/x-sentry-envelope' --data-binary @- \
      "localhost:18080/sentry/api/$id/envelope/" >/dev/null

  log "OTLP/HTTP span"
  curl -fsS -XPOST "${auth[@]}" -H 'content-type: application/json' localhost:18080/v1/traces -d "{\"resourceSpans\":[{
    \"resource\":{\"attributes\":[{\"key\":\"service.name\",\"value\":{\"stringValue\":\"kind-http\"}}]},
    \"scopeSpans\":[{\"spans\":[{\"traceId\":\"$trace\",\"spanId\":\"$span\",\"name\":\"GET /kind\",\"kind\":2,
      \"startTimeUnixNano\":\"${now}000000000\",\"endTimeUnixNano\":\"${now}050000000\",\"status\":{\"code\":2}}]}]}]}" >/dev/null

  log "OTLP/gRPC through the Service (telemetrygen)"
  local hdr=()
  [ -n "$ingest" ] && hdr=(--otlp-header "Authorization=\"Bearer $ingest\"")
  kubectl run telemetrygen --rm -i --restart=Never --image="$telemetrygen" -- \
    traces --otlp-endpoint mallard:4317 --otlp-insecure --traces 3 --service kind-grpc "${hdr[@]}"

  log "trace $trace has the span and the error"
  has_trace() {
    kubectl exec deploy/mallard -- mallard --url http://localhost:8080 --token "$token" --format json trace "$trace" |
      jq -e '(.spans | length) >= 1 and (.errors | length) >= 1' >/dev/null
  }
  retry 30 has_trace
  log "gRPC spans arrived"
  retry 30 sh -c "curl -fsS -H 'Authorization: Bearer $token' 'localhost:18080/otel/api/v1/traces?service=kind-grpc&since=1h' | jq -e '(.traces // .items // .) | length > 0' >/dev/null"

  log "metrics and status"
  curl -fsS localhost:19090/metrics | grep -q '^# TYPE'
  kubectl exec deploy/mallard -- mallard --url http://localhost:8080 --token "$token" status
  last_trace=$trace
}

uninstall() {
  cleanup
  helm uninstall mallard --wait
  kubectl delete pvc --all --wait=true
}

# --- duckdb: everything on the PVC -----------------------------------------
install duckdb -f "$chart/ci/duckdb-values.yaml"
port_forward
check

log "restart keeps the data (PVC, Recreate)"
kubectl rollout restart deploy/mallard
kubectl rollout status deploy/mallard --timeout=5m
port_forward
retry 15 sh -c "kubectl exec deploy/mallard -- mallard --url http://localhost:8080 --token $token --format json trace $last_trace | jq -e '.spans | length >= 1' >/dev/null"
uninstall

# --- s3: RustFS + Postgres catalog, ingest token ---------------------------
log "storage: RustFS + Postgres"
kubectl apply -f "$here/storage.yaml"
kubectl -n storage rollout status deploy/rustfs --timeout=3m
kubectl -n storage rollout status deploy/postgres --timeout=3m
kubectl -n storage wait --for=condition=complete job/create-bucket --timeout=3m
kubectl create secret generic s3-keys \
  --from-literal=AWS_ACCESS_KEY_ID=kind-access-key --from-literal=AWS_SECRET_ACCESS_KEY=kind-secret-key

install s3 -f "$chart/ci/minio-values.yaml" \
  --set storage.s3.endpoint=http://rustfs.storage:9000 \
  --set catalog.postgres.connectionString="host=postgres.storage port=5432 dbname=ducklake user=mallard password=kind-pw"
port_forward
check ingest-token
uninstall
log "ok"
