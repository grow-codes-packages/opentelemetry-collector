# Functions

The OpenTelemetry Collector API of this SDK. Every function exists in all five languages with the
same behaviour, named per language convention (`initTelemetry` in TypeScript and Java,
`init_telemetry` in Python and Rust, `InitTelemetry` in Go). Each page shows every function with an
example per language.

| Page | Functions |
| - | - |
| [Client and health](functions/client.md) | `fromEnv`, `health`, `waitUntilReady`, `ping`, `serverVersion`, `close`, `CollectorHealthClient` |
| [One-call telemetry](functions/telemetry.md) | `initTelemetry` → `TelemetryHandle` (`tracer`, `logger`, `meter`, instruments, `forceFlush`, `shutdown`), resource detection |
| [Tracing setup](functions/tracing-setup.md) | `initTracing`, `tracingConfigFrom`, `resolveTracesEndpoint`, the OTLP span exporter factory |
| [Logs](functions/logs.md) | `logClient` / `createLogClient`, `logExporter` / `OtlpLogExporter`, `logger`, framework adapters |
| [Metrics](functions/metrics.md) | `meter`, `counter`, `upDownCounter`, `histogram`, `gauge`, `createMetricExporter`, `createMeterProvider`, `registerRuntimeMetrics` |
| [Trace and log queries](functions/query.md) | `traceQuery`, `logQuery` (always `UNSUPPORTED`: the collector stores nothing) |

## Overview

| Function | Description | Integration test (Docker) |
| - | - | - |
| `health()` | `health_check` on 13133, zpages fallback on 55679 | test collector and stock collector (zpages fallback) |
| `waitUntilReady(timeout)` | polls `health()` | both collectors |
| `ping()` | empty OTLP trace export with the configured headers | HTTP and gRPC; `bearertokenauth` receiver: 401 / gRPC 16 → `AUTHENTICATION` |
| `serverVersion()` | zpages servicez "Version", then `target_info` | `0.165.0` from both collectors |
| `close()` | flushes and shuts down owned handles | every suite |
| `initTelemetry()` | traces + logs + metrics, one resource | spans read back through Jaeger and the file exporter, log records and metric points through the file exporter |
| `initTracing()` | OTLP span exporter with collector defaults | `tracing.tree`, `tracing.services` conformance + read-back |
| `logClient()` / `createLogClient()` | batching OTLP log client | `logging.roundtrip`, `logging.batch`, `logging.redaction` conformance |
| `logExporter()` | bare OTLP log exporter | HTTP and gRPC, gzip, 64 KiB body limit → `VALIDATION` |
| metric helpers | instruments on the handle and global provider | sum, gauge and histogram values read back exactly |
| `traceQuery()` / `logQuery()` | `UNSUPPORTED` | asserted against the real collector |

Cases the collector container cannot produce (HTTP 503 health, retryable statuses, partial
success, TLS, Kubernetes detection) are covered by unit tests; see
[Compatibility](compatibility.md).

## Service configuration

The full list of settings and environment variables is on the
[Configuration](configuration.md) page. The most used ones:

| Setting | Default | Environment variables (precedence order) |
| - | - | - |
| `url` | `http://127.0.0.1:4318` | `GROW_OPENTELEMETRY_COLLECTOR_URL`, `GROW_OTEL_URL`, `OTEL_EXPORTER_OTLP_ENDPOINT` (http/protobuf) |
| `protocol` | `http/protobuf` | `…_PROTOCOL`, `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc`, `http/protobuf`) |
| `grpcUrl` | scheme and host of `url`, port 4317 | `…_GRPC_URL`, `OTEL_EXPORTER_OTLP_ENDPOINT` (grpc) |
| `headers` | none (redacted) | `…_HEADERS`, `OTEL_EXPORTER_OTLP_HEADERS` |
| `serviceName` | `unknown_service:<process>` | `…_SERVICE_NAME`, `OTEL_SERVICE_NAME` |

`…` stands for `GROW_OPENTELEMETRY_COLLECTOR` (first) and the alias `GROW_OTEL` (second).
