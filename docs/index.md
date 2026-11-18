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

- **Client** with health, authenticated ping and server version ([Client](client.md)).
- **Configuration** from code or `GROW_OPENTELEMETRY_COLLECTOR_*` / vendor environment variables, immutable
  and validated ([Configuration](configuration.md)).
- **Retries** with full-jitter backoff and `Retry-After`, connect/request/total timeouts and
  cancellation in every language.
- **Typed errors** with one set of codes across languages ([Error handling](errors.md)).
- **Redaction everywhere**: credentials never appear in errors, logs, hooks or `toString`
  ([Redaction](redaction.md)).
- **Logging**: batching log client, logger facade, redaction processor, framework adapters and
  log search ([Logging](logging.md)).
- **Tracing**: OpenTelemetry setup, span helpers, propagation, framework adapters and trace
  queries ([Tracing](tracing.md)).
- **Service functions** for the OpenTelemetry Collector API ([Functions](functions.md)).

Start with [Install](install.md) and the [Quickstart](quickstart.md).

!!! note "Independent project"
    This SDK is made by [Grow Codes](https://grow.codes). OpenTelemetry Collector is a trademark of
    The Linux Foundation (OpenTelemetry is a CNCF project). This project is not affiliated with or endorsed by The Linux Foundation (OpenTelemetry is a CNCF project). See
    [License & disclaimer](license.md).
