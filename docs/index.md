# OpenTelemetry Collector SDK

[![Grow Codes](https://img.shields.io/badge/Grow%20Codes-grow.codes-2ea44f)](https://grow.codes)
[![CI](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml/badge.svg)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](license.md)

One API for **OpenTelemetry Collector** in five languages: TypeScript, Rust, Go, Python and Java. Every package
has the same functions, options, defaults, error codes and behaviour, tested against a real
`otel/opentelemetry-collector-contrib:0.165.0` in Docker.

| Language | Package | Coverage |
| - | - | - |
| TypeScript | [`@grow-codes/opentelemetry-collector`](https://www.npmjs.com/package/@grow-codes/opentelemetry-collector) | ![TypeScript coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-typescript.json) |
| Rust | [`grow-opentelemetry-collector`](https://crates.io/crates/grow-opentelemetry-collector) | ![Rust coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-rust.json) |
| Go | [`github.com/grow-codes-packages/opentelemetry-collector/go`](https://pkg.go.dev/github.com/grow-codes-packages/opentelemetry-collector/go) | ![Go coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-go.json) |
| Python | [`grow-opentelemetry-collector`](https://pypi.org/project/grow-opentelemetry-collector/) | ![Python coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-python.json) |
| Java | [`codes.grow.packages:opentelemetry-collector`](https://central.sonatype.com/artifact/codes.grow.packages/opentelemetry-collector) | ![Java coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-java.json) |

## What you get

- **One-call telemetry**: `initTelemetry` sets up traces, logs and metrics over OTLP (gRPC or
  HTTP/protobuf) on the official OpenTelemetry SDK, with one resource (host, process, container,
  Kubernetes) — the generic package for every Grow stack, since the platform deploys the collector
  with each one ([One-call telemetry](functions/telemetry.md)).
- **Standard configuration**: the `OTEL_*` variables work unchanged; `GROW_OPENTELEMETRY_COLLECTOR_*`
  (alias `GROW_OTEL_*`) and explicit values take precedence ([Configuration](configuration.md)).
- **Collector health**: `health_check` with a zpages fallback for stock collectors, `waitUntilReady`,
  an authenticated `ping` and the collector version ([Client and health](functions/client.md)).
- **Logs** through a batching OTLP log client with a logger facade, redaction and framework adapters
  ([Logs](functions/logs.md), [Logging](logging.md)).
- **Metrics** helpers (`meter`, `counter`, `upDownCounter`, `histogram`, `gauge`) and runtime
  metrics ([Metrics](functions/metrics.md)).
- **Tracing** setup with collector defaults, span helpers, propagation and framework adapters
  ([Tracing setup](functions/tracing-setup.md), [Tracing](tracing.md)).
- **Typed errors** with one set of codes across languages, OTLP and gRPC statuses mapped
  ([Error handling](errors.md)); **redaction everywhere**: header credentials never appear in errors,
  logs, hooks or `toString` ([Redaction](redaction.md)).

Start with [Install](install.md) and the [Quickstart](quickstart.md).

!!! note "Independent project"
    This SDK is made by [Grow Codes](https://grow.codes). OpenTelemetry Collector is a trademark of
    The Linux Foundation (OpenTelemetry is a CNCF project). This project is not affiliated with or endorsed by The Linux Foundation (OpenTelemetry is a CNCF project). See
    [License & disclaimer](license.md).
