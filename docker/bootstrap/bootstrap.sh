#!/bin/sh
# Bootstrap for the OpenTelemetry Collector fixture (GRO-1382). POSIX sh, runs in curlimages/curl.
# Idempotent: every run sends a fresh canary, re-proves every read-back path and atomically rewrites
# /bootstrap/env. Nothing secret exists in this fixture (the collector has no auth).
set -eu

C="${COLLECTOR_HOST:-opentelemetry-collector}"
S="${COLLECTOR_STOCK_HOST:-opentelemetry-collector-stock}"
J="${JAEGER_HOST:-jaeger}"
CAP="${CAPTURE_HOST:-capture}"
OUT="${BOOTSTRAP_DIR:-/bootstrap}"
DEADLINE="${BOOTSTRAP_TIMEOUT_S:-90}"

log() { echo "bootstrap: $*" >&2; }
fail() { log "FAIL: $*"; exit 1; }
hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }

# wait_for <label> <cmd...>: retry a command once a second until it succeeds or DEADLINE passes.
wait_for() {
  label=$1; shift; i=0
  until "$@" >/dev/null 2>&1; do
    i=$((i + 1)); [ "$i" -ge "$DEADLINE" ] && fail "timeout waiting for $label"
    sleep 1
  done
  log "ok: $label (${i}s)"
}

wait_for "collector health" curl -fsS "http://$C:13133/"
wait_for "collector OTLP HTTP" sh -c "curl -s -o /dev/null -w '%{http_code}' http://$C:4318/v1/traces | grep -q 405"
wait_for "stock collector OTLP HTTP" sh -c "curl -s -o /dev/null -w '%{http_code}' http://$S:4318/v1/traces | grep -q 405"
wait_for "jaeger query" curl -fsS "http://$J:16686/api/v3/services"
wait_for "capture http" curl -fsS "http://$CAP:8080/ready.txt"

RUN="$(hex 4)"
TRACE_ID="$(hex 16)"
SPAN_ID="$(hex 8)"
CANARY="grow-bootstrap-$RUN"
NOW_NS="$(date +%s)000000000"
END_NS="$(( $(date +%s) + 1 ))000000000"
RES='{"attributes":[{"key":"service.name","value":{"stringValue":"grow-bootstrap"}}]}'

post() { # post <url> <json>  -> fails unless HTTP 200
  code=$(curl -sS -o /tmp/resp -w '%{http_code}' -H 'Content-Type: application/json' --data-binary "$2" "$1")
  [ "$code" = "200" ] || fail "POST $1 -> HTTP $code: $(head -c 300 /tmp/resp)"
}

post "http://$C:4318/v1/traces" "{\"resourceSpans\":[{\"resource\":$RES,\"scopeSpans\":[{\"scope\":{\"name\":\"bootstrap\"},\"spans\":[{\"traceId\":\"$TRACE_ID\",\"spanId\":\"$SPAN_ID\",\"name\":\"bootstrap-canary\",\"kind\":1,\"startTimeUnixNano\":\"$NOW_NS\",\"endTimeUnixNano\":\"$END_NS\",\"attributes\":[{\"key\":\"grow.canary\",\"value\":{\"stringValue\":\"$CANARY\"}}]}]}]}]}"
post "http://$C:4318/v1/logs" "{\"resourceLogs\":[{\"resource\":$RES,\"scopeLogs\":[{\"scope\":{\"name\":\"bootstrap\"},\"logRecords\":[{\"timeUnixNano\":\"$NOW_NS\",\"severityNumber\":9,\"severityText\":\"INFO\",\"body\":{\"stringValue\":\"canary $CANARY\"},\"traceId\":\"$TRACE_ID\",\"spanId\":\"$SPAN_ID\"}]}]}]}"
post "http://$C:4318/v1/metrics" "{\"resourceMetrics\":[{\"resource\":$RES,\"scopeMetrics\":[{\"scope\":{\"name\":\"bootstrap\"},\"metrics\":[{\"name\":\"grow_bootstrap_canary\",\"gauge\":{\"dataPoints\":[{\"timeUnixNano\":\"$NOW_NS\",\"asInt\":\"1\",\"attributes\":[{\"key\":\"run\",\"value\":{\"stringValue\":\"$CANARY\"}}]}]}}]}]}]}"
post "http://$S:4318/v1/traces" "{\"resourceSpans\":[{\"resource\":$RES,\"scopeSpans\":[{\"spans\":[{\"traceId\":\"$TRACE_ID\",\"spanId\":\"$SPAN_ID\",\"name\":\"bootstrap-canary-stock\",\"startTimeUnixNano\":\"$NOW_NS\",\"endTimeUnixNano\":\"$END_NS\"}]}]}]}"
log "canary $CANARY sent (trace $TRACE_ID)"

wait_for "trace in file capture" sh -c "curl -fsS http://$CAP:8080/traces.jsonl | grep -q '$CANARY'"
wait_for "log in file capture" sh -c "curl -fsS http://$CAP:8080/logs.jsonl | grep -q '$CANARY'"
wait_for "metric in file capture" sh -c "curl -fsS http://$CAP:8080/metrics.jsonl | grep -q '$CANARY'"
wait_for "metric on prometheus exporter" sh -c "curl -fsS http://$C:8889/metrics | grep -q '$CANARY'"
wait_for "trace in jaeger" sh -c "curl -fsS http://$J:16686/api/v3/traces/$TRACE_ID | grep -q '$CANARY'"

# Auth-test receiver: empty export with the test bearer token -> 200, without -> 401.
TOKEN="${OTELCOL_TEST_BEARER_TOKEN:?OTELCOL_TEST_BEARER_TOKEN must be set}"
code=$(curl -sS -o /dev/null -w '%{http_code}' -H 'Content-Type: application/x-protobuf' -H "Authorization: Bearer $TOKEN" --data-binary '' "http://$C:4320/v1/traces")
[ "$code" = "200" ] || fail "auth receiver with token -> HTTP $code"
code=$(curl -sS -o /dev/null -w '%{http_code}' -H 'Content-Type: application/x-protobuf' --data-binary '' "http://$C:4320/v1/traces")
[ "$code" = "401" ] || fail "auth receiver without token -> HTTP $code (want 401)"
log "ok: auth receiver (200 with token, 401 without)"

# Server version: the collector's own telemetry carries service_version on target_info.
VERSION=$(curl -fsS "http://$C:8888/metrics" | sed -n 's/^target_info{.*service_version="\([^"]*\)".*/\1/p' | head -n1)
[ -n "$VERSION" ] || fail "no service_version in target_info on :8888/metrics"

mkdir -p "$OUT"
cat > "$OUT/env.tmp" <<EOF
GROW_OPENTELEMETRY_COLLECTOR_URL=http://$C:4318
GROW_OPENTELEMETRY_COLLECTOR_GRPC_URL=http://$C:4317
GROW_OPENTELEMETRY_COLLECTOR_HEALTH_URL=http://$C:13133/
GROW_OPENTELEMETRY_COLLECTOR_ZPAGES_URL=http://$C:55679
GROW_OPENTELEMETRY_COLLECTOR_TELEMETRY_URL=http://$C:8888/metrics
OTEL_EXPORTER_OTLP_ENDPOINT=http://$C:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTELCOL_TEST_SERVER_VERSION=$VERSION
OTELCOL_TEST_GRPC_TARGET=$C:4317
OTELCOL_TEST_CAPTURE_URL=http://$CAP:8080
OTELCOL_TEST_CAPTURE_DIR=/capture
OTELCOL_TEST_PROM_EXPORTER_URL=http://$C:8889/metrics
OTELCOL_TEST_JAEGER_URL=http://$J:16686
OTELCOL_TEST_PPROF_URL=http://$C:1777
OTELCOL_TEST_AUTH_URL=http://$C:4320
OTELCOL_TEST_AUTH_GRPC_URL=http://$C:4319
OTELCOL_TEST_BEARER_TOKEN=$TOKEN
OTELCOL_TEST_STOCK_URL=http://$S:4318
OTELCOL_TEST_STOCK_GRPC_URL=http://$S:4317
OTELCOL_TEST_STOCK_HEALTH_URL=http://$S:13133/
OTELCOL_TEST_BOOTSTRAP_TRACE_ID=$TRACE_ID
OTELCOL_TEST_BOOTSTRAP_CANARY=$CANARY
EOF
chmod 0644 "$OUT/env.tmp"
mv -f "$OUT/env.tmp" "$OUT/env"
log "wrote $OUT/env (collector $VERSION)"
