# Compatibility

## Runtimes

| Language | Supported | Tested in CI |
| - | - | - |
| TypeScript | Node.js ≥ 22.12, ESM and CommonJS | Node 22, 24 |
| Rust | MSRV 1.88, edition 2021, tokio | stable, 1.88 |
| Go | Go ≥ 1.26 | 1.26, 1.27 |
| Python | ≥ 3.10 | 3.10, 3.13, 3.14 |
| Java | ≥ 17 (Java 17 bytecode) | Temurin 17, 21, 25 |

Python 3.10 runs only in CI: the development host has no 3.10 interpreter, so local runs use a
newer Python (the code avoids 3.11+ APIs).

## OpenTelemetry Collector versions

The integration suite runs against `otel/opentelemetry-collector-contrib:0.165.0`, the tag pinned in
the Grow Codes stack catalog, with two collectors: one with the test configuration
(`docker/config/collector.yaml`) and one with the image's **built-in** configuration, exactly as
Grow stacks run it. Exports use plain OTLP (`/v1/traces`, `/v1/logs`, `/v1/metrics` on 4318 and the
OTLP gRPC services on 4317), so any collector or OTLP-compatible backend should work. The admin
endpoints (`health_check` on 13133, zpages on 55679, internal telemetry on 8888) are optional; see
below.

## Functions covered by unit tests only

Every public function is called against the real collector in the Docker suite. These cases cannot
be produced by the collector container, so they are covered by unit tests against mock HTTP and
gRPC servers in all five languages:

| Case | Why the container cannot produce it |
| - | - |
| `health()` with HTTP 503 (`unhealthy`) | `health_check` only answers 503 while the pipelines are not ready, a window the fixture cannot hold open |
| Retries on HTTP 429/502/503/504 and gRPC `UNAVAILABLE`/`RESOURCE_EXHAUSTED`, `Retry-After`, retry exhaustion | the collector accepts everything the test sends |
| Partial success (`rejected` > 0) | the collector reports no rejected records for valid data |
| TLS and mutual TLS (`tls.*`, `https://` endpoints) | the fixture has no TLS listener (decision for v0.1) |
| Kubernetes and container resource detection | the test containers run without a Kubernetes downward API; detectors are tested with fake environments and files |
| `serverVersion()` from `target_info` only | the fixture also exposes zpages; the fallback is tested with mocks (the internal telemetry endpoint is read in Docker too) |

The trace and log **query** clients (`traceQuery()`, `logQuery()`) are called in Docker and
return `UNSUPPORTED` there: the collector stores nothing. The integration tests read telemetry back
through test-only adapters (the collector's file exporter, served over HTTP, and a Jaeger 2.23.0
sidecar); those adapters are not part of the packages.

## Skipped conformance scenarios

| Scenario | Reason |
| - | - |
| — | none: `core.health`, `logging.batch`, `logging.redaction`, `logging.roundtrip`, `tracing.services` and `tracing.tree` pass in every language |

## Collector behaviour the SDK adapts to

| Behaviour of the collector | What the SDK does |
| - | - |
| The image's built-in configuration (used by Grow stacks) binds `health_check` to `localhost:13133`, so it is unreachable from other containers | `health()` falls back to zpages `/debug/servicez` on 55679 (`details.source = "zpages"`); the test fixture overrides `health_check` to `0.0.0.0:13133`. Platform follow-up: bind `health_check` to `0.0.0.0` in Grow stacks |
| No version endpoint | `serverVersion()` parses the "Version" row of zpages `/debug/servicez`, then `service_version` of `target_info` at `telemetryUrl` (`:8888/metrics`); `UNSUPPORTED` when neither answers |
| zpages `tracez` returns 404 in 0.165.0 | not used |
| A body over the receiver's size limit gets HTTP **400**, not 413 | mapped to `VALIDATION` |
| A malformed gRPC message gets `INTERNAL` (13), not `INVALID_ARGUMENT` | mapped to `VALIDATION` (not retryable, per the OTLP rules) |
| No authentication by default | `ping()` sends the configured headers; with `bearertokenauth` or a gateway, 401 / gRPC `UNAUTHENTICATED` → `AUTHENTICATION`, 403 / `PERMISSION_DENIED` → `PERMISSION_DENIED` |
| An empty OTLP export returns 200 and stores nothing | used by `ping()` as the authenticated round trip |
| No native protocol | `protocol: native` → `CONFIG` |

## Error mapping of OTLP responses

HTTP statuses use the shared SDK table ([Error handling](errors.md)); the error message comes from the
`google.rpc.Status` body (protobuf or JSON). gRPC status codes map the same way in every language:

| gRPC status | SDK code | Retryable |
| - | - | - |
| `CANCELLED` (1) | `CANCELLED` | no |
| `INVALID_ARGUMENT` (3), `FAILED_PRECONDITION` (9), `OUT_OF_RANGE` (11), `INTERNAL` (13) | `VALIDATION` | no |
| `DEADLINE_EXCEEDED` (4) | `TIMEOUT` | yes |
| `NOT_FOUND` (5), `UNIMPLEMENTED` (12) | `NOT_FOUND` | no |
| `ALREADY_EXISTS` (6) | `CONFLICT` | no |
| `PERMISSION_DENIED` (7) | `PERMISSION_DENIED` | no |
| `RESOURCE_EXHAUSTED` (8) | `RATE_LIMITED` | yes |
| `UNAUTHENTICATED` (16) | `AUTHENTICATION` | no |
| `UNKNOWN` (2), `ABORTED` (10), `UNAVAILABLE` (14), `DATA_LOSS` (15), any other code | `UNAVAILABLE` | yes |

Errors from OTLP exports keep the **natural** `retryable` flag of their code. The official
exporters have already retried by the OTLP rules (429/502/503/504 and the retryable gRPC codes)
before the error reaches you, so do not wrap an export in a second retry loop.

A call on a **closed** client (health, ping, version, or creating a handle, log client or exporter)
fails with `CANCELLED` in every language.

## Language differences

The API is the same in every language; only the conventions differ (naming, durations, async
model). The differences below are limits of the official OpenTelemetry libraries of a language or of
the generated code:

| Topic | Languages | Difference |
| - | - | - |
| gRPC with `tls.insecureSkipVerify` | Python, Rust | `CONFIG`: grpcio and tonic cannot skip certificate verification. Use a CA bundle (`tls.caCertPath`) or `http/protobuf`. TypeScript, Go and Java support it (tests only) |
| Partial success of gRPC **log** exports | Rust | not reported: the official tonic log exporter drops the response, so `rejected` stays 0 (HTTP log exports and all other languages report it) |
| Custom log severity text | Rust | a severity text other than `TRACE` … `FATAL` becomes the canonical text of its severity number (the Rust OpenTelemetry SDK keeps a `&'static str`) |
| Exporter retries | Rust | the `opentelemetry-otlp` 0.31 exporters do not retry. Retryable log export failures are retried by the SDK's log batch processor; span and metric exports are not retried. The other languages retry inside the official exporters |
| Runtime metrics (`runtimeMetrics: true`) | all | metric names follow each runtime: TypeScript `process.memory.usage`, `v8js.memory.heap.used`, `process.cpu.time`, `nodejs.eventloop.delay.p99`, `process.uptime`; Rust `process.*` and `rust.tokio.workers`, `rust.tokio.alive_tasks`; Go the OpenTelemetry contrib runtime instrumentation (`go.memory.*`, `go.goroutine.count`, …); Python `process.*` and `cpython.gc.collections`; Java `jvm.memory.*`, `jvm.cpu.*`, `jvm.thread.count`, `jvm.class.count`, `process.uptime` |
| `retryable` on errors | Java | the generated exception classes take no flag, so `retryable()` always equals the default of the error code. The other languages accept an override, which this package never uses on the export paths, so all five report the same flags |
| Per-signal endpoint setters | Rust, Java | builder methods take the signal (`signal_url(Signal::Traces, …)`, `signalUrl(Signal.TRACES, …)`) instead of `tracesUrl` / `logsUrl` / `metricsUrl` |
| Closing an `initTelemetry` handle | Python, Java | the handle is also a context manager / `AutoCloseable` (`with …`, try-with-resources) |

Java cannot change the process environment, so the testing helper `loadEnv` sets **system
properties** instead and returns the names it set.
