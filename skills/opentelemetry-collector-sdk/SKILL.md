---
name: opentelemetry-collector-sdk
description: Add and use the Grow Codes SDK for the OpenTelemetry Collector (`opentelemetry-collector`) in TypeScript, Rust, Go, Python or Java projects. Use when code must send traces, logs or metrics to an OpenTelemetry Collector (every Grow stack has one) or check one - installing @grow-codes/opentelemetry-collector, the grow-opentelemetry-collector crate or PyPI package, the Go module github.com/grow-codes-packages/opentelemetry-collector/go or Maven codes.grow.packages:opentelemetry-collector, one-call telemetry with initTelemetry, configuring it with OTEL_* or GROW_OPENTELEMETRY_COLLECTOR_* / GROW_OTEL_* variables, collector health (health_check / zpages), handling OtelCollectorError codes, testing with the bundled fakes, or running the Docker integration suite of this repository.
---

# OpenTelemetry Collector SDK (Grow Codes)

One API for the OpenTelemetry Collector in five languages (categories: logging, tracing), built on
the official OpenTelemetry SDK of each language. The main entry point is **`initTelemetry`**: traces,
logs and metrics over OTLP in one call. Grow stacks always run a collector, so this is the default
way for a Grow service to emit telemetry.

## 1. Add the package

| Language | Command | Import |
| - | - | - |
| TypeScript | `npm install @grow-codes/opentelemetry-collector` | `import { OtelCollectorClient, initTelemetry, withSpan } from "@grow-codes/opentelemetry-collector"` |
| Rust | `cargo add grow-opentelemetry-collector` (add `tokio` with `macros,rt-multi-thread`; feature `blocking` for a sync client) | `use grow_opentelemetry_collector::{OtelCollectorClient, init_telemetry};` |
| Go | `go get github.com/grow-codes-packages/opentelemetry-collector/go` | `otelcollector "github.com/grow-codes-packages/opentelemetry-collector/go"` |
| Python | `pip install grow-opentelemetry-collector` (gRPC: `"grow-opentelemetry-collector[grpc]"`) | `import grow_opentelemetry_collector as sdk` |
| Java | Maven `codes.grow.packages:opentelemetry-collector` (Gradle `implementation("codes.grow.packages:opentelemetry-collector:<version>")`) | `import codes.grow.packages.otelcollector.OtelCollectorClient;` |

Check the latest version in the README badges before pinning one.

## 2. Configure

The standard OpenTelemetry variables work unchanged; prefer them (or the `GROW_` ones) over values in
code:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318   # or GROW_OPENTELEMETRY_COLLECTOR_URL
export OTEL_SERVICE_NAME=checkout                               # or GROW_OPENTELEMETRY_COLLECTOR_SERVICE_NAME
# export OTEL_EXPORTER_OTLP_PROTOCOL=grpc                       # port 4317 (default http/protobuf on 4318)
# export OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer%20<token>"   # only behind an authenticator/gateway
```

Precedence: explicit value > `GROW_OPENTELEMETRY_COLLECTOR_<NAME>` > `GROW_OTEL_<NAME>` > per-signal
`OTEL_EXPORTER_OTLP_<SIGNAL>_*` > generic `OTEL_*` > default. Useful names (after the prefix):
`URL`, `GRPC_URL` (default: host of `URL`, port 4317), `PROTOCOL`, `HEADERS`, `SERVICE_NAME`,
`SERVICE_VERSION`, `ENVIRONMENT`, `RESOURCE_ATTRIBUTES`, `TRACES`/`LOGS`/`METRICS` (on/off),
`SAMPLER`, `PROPAGATORS`, `RUNTIME_METRICS`, `HEALTH_URL`, `ZPAGES_URL`, `TELEMETRY_URL`, plus the
transport settings `CONNECT_TIMEOUT`, `REQUEST_TIMEOUT`, `TOTAL_TIMEOUT`, `MAX_RETRIES`, `CA_CERT`,
`CLIENT_CERT`, `CLIENT_KEY`, `PROXY`. `OTEL_SDK_DISABLED=true` and `OTEL_<SIGNAL>_EXPORTER=none`
switch signals off. Header values are credentials: they are redacted everywhere, never commit them.

## 3. Use

### One-call telemetry (start here)

Call it once at startup and shut it down on exit (or let `client.close()` do it):

| Language | Code |
| - | - |
| TypeScript | `const t = initTelemetry(); … await t.shutdown();` |
| Rust | `let t = init_telemetry(&OtelCollectorConfig::from_env()?)?; … t.shutdown().await?;` |
| Go | `t, err := otelcollector.InitTelemetryFromEnv(ctx, otelcollector.Config{}); defer t.Shutdown(ctx)` |
| Python | `with sdk.init_telemetry() as t: …` |
| Java | `try (TelemetryHandle t = OtelCollectorTelemetry.initTelemetry()) { … }` |

The handle gives `tracer(name)`, `logger(name)` (records carry `trace_id`/`span_id`), `meter(name)`,
`counter`, `upDownCounter`, `histogram`, `gauge`, `forceFlush()` and `shutdown()`. Providers and
propagators are registered globally (`setGlobal`, default on), so the span helpers (`withSpan`,
`traced`, `currentTraceId`, `inject`/`extract`, `traceFields`) and framework middleware pick them up.
Call `forceFlush()` before a short-lived process exits; telemetry is batched.

### Collector checks

`OtelCollectorClient.fromEnv()` → `health()` (never throws for an unhealthy collector; falls back to
zpages because stock collectors bind `health_check` to localhost), `waitUntilReady(timeout)`,
`ping()` (empty OTLP export with your headers: `AUTHENTICATION` when an authenticator rejects them),
`serverVersion()` (`UNSUPPORTED` when zpages and `telemetryUrl` are unavailable) and `close()`
(flushes everything the client created; later calls fail with `CANCELLED`).

### Logs, metrics, tracing on their own

- Logs: `client.logClient()` / `createLogClient(config)` → `send`, `sendBatch`, `logger(...)`,
  `flush`, `stats`, `close`; framework adapters (pino/winston, `tracing`/`log`, `slog`/zap,
  `logging`/structlog, Logback/Log4j2/JUL). Partial success shows as `rejected` + `VALIDATION`.
- Metrics: global `meter`, `counter`, `upDownCounter`, `histogram` (`boundaries`), `gauge`;
  `runtimeMetrics: true` adds runtime metrics (names differ per runtime).
- Tracing only: `initTracing({ serviceName })` / `client.initTracing(...)` with collector defaults.
- Queries: the collector stores nothing; `traceQuery()` / `logQuery()` always fail with
  `UNSUPPORTED`. Read telemetry back from the backend the collector exports to.

<!-- service:skill-usage — the service adds its own functions and examples here. -->

### Errors

Errors carry `code` (`CONFIG`, `AUTHENTICATION`, `PERMISSION_DENIED`, `NOT_FOUND`, `CONFLICT`,
`RATE_LIMITED`, `VALIDATION`, `UNAVAILABLE`, `TIMEOUT`, `UNSUPPORTED`, `CANCELLED`), `retryable`,
`httpStatus` and `requestId`. Branch on the code, not on the message (Go:
`errors.Is(err, otelcollector.ErrUnsupported)`). OTLP export errors were already retried by the
official exporters; do not wrap exports in another retry loop. `protocol: native` is `CONFIG`; gRPC
with `insecureSkipVerify` is `CONFIG` in Python and Rust.

## 4. Test code that uses the SDK

Use the in-memory fakes from the testing module instead of a real collector in unit tests
(memory span/log exporters, fake clocks, the conformance runner): TypeScript
`@grow-codes/opentelemetry-collector/testing`, Rust feature `testing`, Go package `testkit`, Python
`grow_opentelemetry_collector.testing`, Java `codes.grow.packages.otelcollector.testing`. Pass
`setGlobal: false` to `initTelemetry` in tests that must not touch the global providers.

## 5. Work on this repository

Read `AGENTS.md` first. Short version: the five languages keep the same API; files with the
`Generated by sdk-kit` header are changed in sdk-kit, not here; `make lint coverage` must pass
(≥ 90 % lines and branches); `make test-docker LANGS="<lang>"` runs the integration suite against
`otel/opentelemetry-collector-contrib:0.165.0` (test collector with file exporters + Jaeger sidecar,
plus a stock collector). Collector quirks and per-language limits: `docs/compatibility.md`.
