# Grow Codes SDK for OpenTelemetry Collector

[![CI](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml/badge.svg)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/grow-codes-packages/opentelemetry-collector?sort=semver)](https://github.com/grow-codes-packages/opentelemetry-collector/releases)
[![Docs](https://img.shields.io/badge/docs-grow--codes--packages.github.io-blue)](https://grow-codes-packages.github.io/opentelemetry-collector/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Grow Codes](https://img.shields.io/badge/Grow%20Codes-grow.codes-2ea44f)](https://grow.codes)

| Language | Package | Coverage |
| - | - | - |
| TypeScript | [![npm](https://img.shields.io/npm/v/@grow-codes/opentelemetry-collector)](https://www.npmjs.com/package/@grow-codes/opentelemetry-collector) | [![TypeScript coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-typescript.json)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml) |
| Rust | [![crates.io](https://img.shields.io/crates/v/grow-opentelemetry-collector)](https://crates.io/crates/grow-opentelemetry-collector) [![docs.rs](https://img.shields.io/docsrs/grow-opentelemetry-collector)](https://docs.rs/grow-opentelemetry-collector) | [![Rust coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-rust.json)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml) |
| Go | [![Go Reference](https://pkg.go.dev/badge/github.com/grow-codes-packages/opentelemetry-collector/go.svg)](https://pkg.go.dev/github.com/grow-codes-packages/opentelemetry-collector/go) | [![Go coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-go.json)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml) |
| Python | [![PyPI](https://img.shields.io/pypi/v/grow-opentelemetry-collector)](https://pypi.org/project/grow-opentelemetry-collector/) | [![Python coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-python.json)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml) |
| Java | [![Maven Central](https://img.shields.io/maven-central/v/codes.grow.packages/opentelemetry-collector)](https://central.sonatype.com/artifact/codes.grow.packages/opentelemetry-collector) | [![Java coverage](https://img.shields.io/endpoint?url=https://grow-codes-packages.github.io/opentelemetry-collector/badges/coverage-java.json)](https://github.com/grow-codes-packages/opentelemetry-collector/actions/workflows/ci.yml) |

Open-source client libraries for the [OpenTelemetry Collector](#trademarks-and-disclaimer) in
**TypeScript, Rust, Go, Python and Java** — one API, five idiomatic packages, tested against a real
OpenTelemetry Collector in Docker. Built on the official OpenTelemetry SDK of each language; the
packages add the collector defaults, configuration, health checks and error model, they do not
re-implement OpenTelemetry.

- **One-call telemetry**: `initTelemetry()` sets up **traces, logs and metrics** over OTLP (gRPC or
  HTTP/protobuf) with one resource (host, process, container, Kubernetes). This is the generic
  package for every Grow stack: the platform deploys an OpenTelemetry Collector with each one.
- **Standard configuration**: the `OTEL_*` variables work unchanged; `GROW_OPENTELEMETRY_COLLECTOR_*`
  (alias `GROW_OTEL_*`) and explicit values take precedence.
- **Collector health**: `health()` on the `health_check` extension with a zpages fallback for stock
  collectors, `waitUntilReady()`, an authenticated `ping()` and `serverVersion()`.
- **Logging**: a batching OTLP log client with back-pressure, redaction, a logger facade and adapters
  for the common logging frameworks of each language.
- **Metrics**: `meter`, `counter`, `upDownCounter`, `histogram`, `gauge` and runtime metrics.
- **Tracing**: OpenTelemetry setup with collector defaults, span helpers, context propagation and
  framework middleware.
- **One error model** (`OtelCollectorError` with canonical codes, OTLP and gRPC statuses mapped) and
  **secrets stay secret**: header credentials are redacted in `toString`, logs, hooks and errors.

## Contents

- [Supported service version](#supported-service-version)
- [Install](#install)
- [Quickstart](#quickstart)
- [Configuration](#configuration)
- [Function overview](#function-overview)
- [Testing with Docker](#testing-with-docker)
- [Compatibility](#compatibility)
- [Contributing](#contributing)
- [Security](#security)
- [Licence](#licence)
- [Trademarks and disclaimer](#trademarks-and-disclaimer)
- [About Grow Codes](#about-grow-codes)

## Supported service version

Tested in CI against **`otel/opentelemetry-collector-contrib:0.165.0`** (the version pinned in the
Grow Codes stack catalog), both with a test configuration and with the image's built-in
configuration as Grow stacks run it. Exports are plain OTLP (port 4318 for HTTP/protobuf, 4317 for
gRPC), so other collector versions and OTLP-compatible backends work too; the admin endpoints
(`health_check` 13133, zpages 55679, internal telemetry 8888) are optional. See
[docs/compatibility.md](docs/compatibility.md).

## Install

| Language | Package | Install |
| - | - | - |
| TypeScript (Node.js ≥ 22.12) | `@grow-codes/opentelemetry-collector` | `npm install @grow-codes/opentelemetry-collector` |
| Rust (MSRV 1.88) | `grow-opentelemetry-collector` | `cargo add grow-opentelemetry-collector` (features: `blocking`, `testing`) |
| Go (1.26+) | `github.com/grow-codes-packages/opentelemetry-collector/go` | `go get github.com/grow-codes-packages/opentelemetry-collector/go` |
| Python (≥ 3.10) | `grow-opentelemetry-collector` | `pip install grow-opentelemetry-collector` (gRPC: `pip install "grow-opentelemetry-collector[grpc]"`) |
| Java (17+) | `codes.grow.packages:opentelemetry-collector` | see below |

Maven:

```xml
<dependency>
  <groupId>codes.grow.packages</groupId>
  <artifactId>opentelemetry-collector</artifactId>
  <version>0.1.0</version>
</dependency>
```

Gradle (Kotlin DSL):

```kotlin
implementation("codes.grow.packages:opentelemetry-collector:0.1.0")
```

## Quickstart

Every package reads the standard OpenTelemetry variables (`OTEL_EXPORTER_OTLP_ENDPOINT`,
`OTEL_SERVICE_NAME`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, …) and the
`GROW_OPENTELEMETRY_COLLECTOR_*` variables (see [Configuration](#configuration)), or takes the values
in code. The examples check the collector, then send a span, a log record and a metric point.

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318   # or GROW_OPENTELEMETRY_COLLECTOR_URL
export OTEL_SERVICE_NAME=checkout
```

### TypeScript

```ts
import { OtelCollectorClient, withSpan } from "@grow-codes/opentelemetry-collector";

const client = OtelCollectorClient.fromEnv();
try {
  const health = await client.waitUntilReady(10_000); // health_check, zpages fallback
  console.log("healthy:", health.healthy, "ping:", await client.ping(), "ms");

  const telemetry = client.initTelemetry(); // traces + logs + metrics over OTLP
  const log = telemetry.logger("checkout");
  const orders = telemetry.counter("orders_total");
  await withSpan("checkout", async () => {
    log.info("order received", { "order.id": "o-42" }); // carries trace_id / span_id
    orders.add(1);
  });
} finally {
  await client.close(); // flushes and shuts the telemetry down
}
```

### Rust

```rust
use std::time::Duration;
use grow_opentelemetry_collector::opentelemetry::trace::Tracer as _;
use grow_opentelemetry_collector::{InstrumentOptions, OtelCollectorClient, TelemetryOptions};

#[tokio::main]
async fn main() -> grow_opentelemetry_collector::Result<()> {
    let client = OtelCollectorClient::from_env()?;
    let health = client.wait_until_ready(Duration::from_secs(10)).await?;
    println!("healthy: {} ping: {:?}", health.healthy, client.ping().await?);

    let telemetry = client.init_telemetry(TelemetryOptions::default())?;
    telemetry.tracer("checkout", None).in_span("checkout", |_cx| {
        telemetry
            .logger(Some("checkout"), Default::default())
            .info("order received", Default::default());
    });
    telemetry.counter("orders_total", InstrumentOptions::default())?.add(1, &[]);
    client.close().await; // flushes and shuts the telemetry down
    Ok(())
}
```

### Go

```go
package main

import (
	"context"
	"fmt"
	"log"
	"time"

	otelcollector "github.com/grow-codes-packages/opentelemetry-collector/go"
)

func main() {
	ctx := context.Background()
	client, err := otelcollector.NewClientFromEnv()
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close() // flushes and shuts the telemetry down

	health, err := client.WaitUntilReady(ctx, 10*time.Second)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("healthy:", health.Healthy, "via", health.Details["source"])

	telemetry, err := client.InitTelemetry(ctx) // traces + logs + metrics over OTLP
	if err != nil {
		log.Fatal(err)
	}
	orders, _ := telemetry.Counter("orders_total", otelcollector.InstrumentOptions{})
	logger := telemetry.Logger("checkout", nil)
	_, _ = otelcollector.WithSpan(ctx, "checkout", func(ctx context.Context) (struct{}, error) {
		logger.Info(ctx, "order received", "order.id", "o-42") // carries trace_id / span_id
		orders.Add(ctx, 1)
		return struct{}{}, nil
	}, otelcollector.WithTracer(telemetry.Tracer("checkout")))
}
```

### Python

```python
import grow_opentelemetry_collector as sdk

with sdk.OtelCollectorClient.from_env() as client:
    health = client.wait_until_ready(10)  # health_check, zpages fallback
    print("healthy:", health.healthy, "ping:", client.ping(), "ms")

    telemetry = client.init_telemetry()  # traces + logs + metrics over OTLP
    orders = telemetry.counter("orders_total")
    with sdk.with_span("checkout", tracer=telemetry.tracer("checkout")):
        telemetry.logger("checkout").info("order received", {"order.id": "o-42"})
        orders.add(1)
# leaving the block flushes and shuts the telemetry down
```

Async code uses `AsyncOtelCollectorClient` with the same methods (`async with`, `await client.ping()`).

### Java

```java
import codes.grow.packages.otelcollector.InstrumentOptions;
import codes.grow.packages.otelcollector.OtelCollectorClient;
import codes.grow.packages.otelcollector.TelemetryHandle;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.context.Scope;
import java.time.Duration;
import java.util.Map;

try (OtelCollectorClient client = OtelCollectorClient.fromEnv()) {
    var health = client.waitUntilReady(Duration.ofSeconds(10));
    System.out.println("healthy: " + health.healthy() + " ping: " + client.ping().toMillis() + " ms");

    TelemetryHandle telemetry = client.initTelemetry(); // traces + logs + metrics over OTLP
    Span span = telemetry.tracer("checkout").spanBuilder("checkout").startSpan();
    try (Scope scope = span.makeCurrent()) {
        telemetry.logger("checkout").info("order received", Map.of("order.id", "o-42"));
        telemetry.counter("orders_total", InstrumentOptions.unit("{order}")).add(1);
    } finally {
        span.end();
    }
} // closing the client flushes and shuts the telemetry down
```

More complete programs live in [examples/](examples/) — one folder per language.

## Configuration

All settings can be passed in code or through environment variables. Precedence: **explicit value >
`GROW_OPENTELEMETRY_COLLECTOR_<NAME>` > `GROW_OTEL_<NAME>` > per-signal `OTEL_*` > generic `OTEL_*` >
default.** Map settings (`headers`, resource attributes, batch settings) merge key by key. A present
but invalid value raises a `ConfigError` that names the variable (never its value).

| Setting | `GROW_OPENTELEMETRY_COLLECTOR_…` / `GROW_OTEL_…` | Standard variable | Default |
| - | - | - | - |
| `url` (OTLP/HTTP base) | `URL` | `OTEL_EXPORTER_OTLP_ENDPOINT` (http/protobuf) | `http://127.0.0.1:4318` |
| `grpcUrl` (OTLP/gRPC target) | `GRPC_URL` | `OTEL_EXPORTER_OTLP_ENDPOINT` (grpc) | scheme and host of `url`, port 4317 |
| `protocol` | `PROTOCOL` | `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` (`grpc`; `native` → `CONFIG`) |
| `tracesUrl` / `logsUrl` / `metricsUrl` (used as is, both protocols) | `TRACES_URL` / `LOGS_URL` / `METRICS_URL` | `OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT` | derived |
| `headers` (redacted) | `HEADERS` | `OTEL_EXPORTER_OTLP_HEADERS` | none |
| per-signal headers | — | `OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS` | none |
| `compression` | `COMPRESSION` | `OTEL_EXPORTER_OTLP_COMPRESSION` | `gzip` |
| `exportTimeout` | `EXPORT_TIMEOUT` | `OTEL_EXPORTER_OTLP_TIMEOUT` | 10 s |
| `healthUrl` / `zpagesUrl` / `telemetryUrl` | `HEALTH_URL` / `ZPAGES_URL` / `TELEMETRY_URL` | — | host of `url` on 13133 / 55679 / none |
| `serviceName` | `SERVICE_NAME` | `OTEL_SERVICE_NAME` | `unknown_service:<process>` |
| `serviceVersion` / `environment` | `SERVICE_VERSION` / `ENVIRONMENT` | — | none |
| `resourceAttributes` | `RESOURCE_ATTRIBUTES` | `OTEL_RESOURCE_ATTRIBUTES` | none |
| `resourceDetectors` | `RESOURCE_DETECTORS` | — | `host,process,container,k8s` |
| `traces` / `logs` / `metrics` | `TRACES` / `LOGS` / `METRICS` | `OTEL_<SIGNAL>_EXPORTER=none`, `OTEL_SDK_DISABLED` | `true` |
| `sampler` / `samplerRatio` | `SAMPLER` / `SAMPLER_RATIO` | `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG` | `parentbased_always_on` / 1.0 |
| `propagators` | `PROPAGATORS` | `OTEL_PROPAGATORS` | `tracecontext,baggage` |
| span / log batch settings | — | `OTEL_BSP_*` / `OTEL_BLRP_*` | SDK defaults |
| `metricExportInterval` | `METRIC_EXPORT_INTERVAL` | `OTEL_METRIC_EXPORT_INTERVAL` | 60 s |
| `runtimeMetrics` | `RUNTIME_METRICS` | — | `false` |
| `connectTimeout` / `requestTimeout` / `totalTimeout` | `CONNECT_TIMEOUT` / `REQUEST_TIMEOUT` / `TOTAL_TIMEOUT` | — | 10 s / 30 s / 120 s |
| `retry.maxRetries` | `MAX_RETRIES` | — | 3 |
| `tls.caCertPath` | `CA_CERT` | `OTEL_EXPORTER_OTLP_CERTIFICATE` | system roots |
| `tls.clientCertPath` / `tls.clientKeyPath` | `CLIENT_CERT` / `CLIENT_KEY` | `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` / `…_CLIENT_KEY` | none |
| `tls.insecureSkipVerify` (tests only) | `INSECURE_SKIP_VERIFY` | — | `false` |
| `proxy` | `PROXY` | — | none (environment proxies are ignored) |
| `userAgentSuffix`, `hooks`, `logger` | — | — | none / silent |

Durations accept milliseconds (`1500`) or units (`500ms`, `10s`, `1m30s`); booleans accept
`true/false/1/0/yes/no/on/off`. Field names follow each language's convention (`request_timeout` in
Python and Rust, `RequestTimeout` in Go). The full reference is in
[docs/configuration.md](docs/configuration.md).

## Function overview

<!-- service:function-overview — the service fills this section; keep the heading. -->

The same functions exist in every language, named per language convention (`initTelemetry` in
TypeScript and Java, `init_telemetry` in Python and Rust, `InitTelemetry` in Go). Every function has a
page with all five languages in the [documentation](https://grow-codes-packages.github.io/opentelemetry-collector/functions/).

### Client and health

| Function | Description |
| - | - |
| `fromEnv()` / constructor | build from the environment or an explicit config |
| `health()` | `health_check` on 13133, falls back to zpages on 55679 (stock collectors bind `health_check` to localhost); never throws for an unhealthy or unreachable collector |
| `waitUntilReady(timeout)` | polls `health()` until healthy; `TIMEOUT` otherwise |
| `ping()` | empty OTLP trace export with the configured headers (stores nothing); returns the latency |
| `serverVersion()` | zpages servicez "Version", then `target_info`; e.g. `0.165.0` |
| `close()` | idempotent; flushes and shuts down every handle, log client and exporter made by the client |
| `CollectorHealthClient` | the health functions on their own |

| TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - |
| `await client.waitUntilReady(10_000)` | `client.wait_until_ready(Duration::from_secs(10)).await?` | `client.WaitUntilReady(ctx, 10*time.Second)` | `client.wait_until_ready(10)` | `client.waitUntilReady(Duration.ofSeconds(10))` |

### One-call telemetry

| Function | Description |
| - | - |
| `initTelemetry(config, options)` / `client.initTelemetry()` | traces + logs + metrics over OTLP, one resource |
| `TelemetryHandle`: `tracer`, `logger`, `meter`, `counter`, `upDownCounter`, `histogram`, `gauge` | instruments of the handle's providers; loggers carry `trace_id` / `span_id` |
| `TelemetryHandle`: `forceFlush`, `shutdown` | flush all three signals; stop everything (idempotent) |
| `detectResourceAttributes(detectors)` | `host`, `process`, `container`, `k8s` attributes |

| TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - |
| `const t = client.initTelemetry()` | `let t = client.init_telemetry(TelemetryOptions::default())?;` | `t, err := client.InitTelemetry(ctx)` | `t = client.init_telemetry()` | `TelemetryHandle t = client.initTelemetry();` |

### Tracing setup

| Function | Description |
| - | - |
| `initTracing(tracingConfig)` / `client.initTracing()` | OTLP span exporter with collector defaults (`TracingHandle`) |
| `tracingConfigFrom(collector, tracing)`, `resolveTracesEndpoint` | effective tracing config; HTTP endpoints without a path get `/v1/traces` |
| span helpers | `withSpan`, `traced`, `currentTraceId`, `addEvent`, `recordException`, `setError`, `inject` / `extract`, `traceFields` ([docs/tracing.md](docs/tracing.md)) |

| TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - |
| `initTracing({ serviceName: "checkout" })` | `init_tracing(TracingConfig::new("checkout"))?` | `otelcollector.InitTracing(ctx, otelcollector.TracingConfig{ServiceName: "checkout"})` | `sdk.init_tracing(service_name="checkout")` | `OtelCollectorTracing.initTracing(TracingConfig.builder("checkout").build())` |

### Logs

| Function | Description |
| - | - |
| `client.logClient()` / `createLogClient(config)` | batching OTLP log client: `send`, `sendBatch`, `flush`, `close`, `stats`, `logger` |
| `client.logExporter()` / `OtlpLogExporter` | bare exporter: `export(records)` → `{accepted, rejected, error}` (partial success → `rejected`) |
| framework adapters | pino, winston · `tracing` layer, `log` bridge · `slog`, zap · `logging`, structlog · Logback, Log4j2, JUL |

| TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - |
| `client.logClient().logger("app").info("hi")` | `client.log_client(LogClientOptions::default())?.send("hi".into())` | `lc.Logger("app", nil).Info(ctx, "hi")` | `client.log_client().logger("app").info("hi")` | `client.logClient().logger("app").info("hi")` |

### Metrics

| Function | Description |
| - | - |
| `meter`, `counter`, `upDownCounter`, `histogram`, `gauge` | instruments on the global provider (or the handle's) with `unit`, `description`, histogram `boundaries` |
| `createMetricExporter(config)`, `createMeterProvider(config)` | the OTLP metric exporter / a meter provider with a periodic reader |
| `registerRuntimeMetrics(meter)` | runtime metrics (`runtimeMetrics: true`); names follow each runtime |

| TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - |
| `counter("orders").add(1)` | `counter("orders", InstrumentOptions::default())?.add(1, &[])` | `c, _ := otelcollector.Counter("orders", otelcollector.InstrumentOptions{}); c.Add(ctx, 1)` | `sdk.counter("orders").add(1)` | `OtelCollectorMetrics.counter("orders").add(1)` |

### Trace and log queries

The collector stores nothing, so `client.traceQuery()` and `client.logQuery()` (the category query
interfaces) fail with `UNSUPPORTED` in every language. Query the backend the collector exports to.

| TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - |
| `client.traceQuery().getTrace(id)` → `UnsupportedError` | `client.trace_query().get_trace(&id).await` → `UNSUPPORTED` | `client.TraceQuery().GetTrace(ctx, id)` → `ErrUnsupported` | `client.trace_query().get_trace(id)` → `UnsupportedError` | `client.traceQuery().getTrace(id)` → `UnsupportedException` |

## Testing with Docker

The integration suite runs every public function against a real OpenTelemetry Collector
(`otel/opentelemetry-collector-contrib:0.165.0`) in Docker: one collector with a test configuration
(file exporters served over HTTP, a Jaeger 2.23.0 sidecar to read traces back, a second OTLP receiver
behind `bearertokenauth`) and one with the image's built-in configuration. Each language package is
packed and installed into a separate consumer project first, so the tests use the package exactly as
you would.

```sh
make test-docker                     # all five languages
make test-docker LANGS="go python"   # a subset
```

No host ports are published and every run uses its own Compose project, so runs never collide; the
stack is always removed afterwards (`docker compose down -v`). Unit tests need no Docker:
`make test` or `make coverage`.

## Compatibility

| | TypeScript | Rust | Go | Python | Java |
| - | - | - | - | - | - |
| Runtime | Node.js ≥ 22.12, ESM + CJS | stable, MSRV 1.88 | 1.26, 1.27 | 3.10 – 3.14 | 17, 21, 25 |
| Async | `Promise` + `AbortSignal` | `async` (tokio), `blocking` feature | `context.Context` | sync + `asyncio` | sync + `CompletableFuture` |
| OpenTelemetry SDK | `@opentelemetry/*` 2.x / 0.2xx | `opentelemetry` 0.31 | `go.opentelemetry.io/otel` 1.47 | `opentelemetry-sdk` ≥ 1.27 | `io.opentelemetry` (BOM) |
| OTLP gRPC + HTTP/protobuf | yes | yes | yes | yes (gRPC with the `grpc` extra) | yes |
| OpenTelemetry Collector | `0.165.0` | `0.165.0` | `0.165.0` | `0.165.0` | `0.165.0` |

Per-language limits (for example: Rust does not report the partial success of gRPC log exports,
gRPC with `insecureSkipVerify` is a `CONFIG` error in Python and Rust, runtime metric names differ per
runtime) and the cases covered by unit tests only are listed in
[docs/compatibility.md](docs/compatibility.md).

## Contributing

Contributions are welcome — please read [CONTRIBUTING.md](CONTRIBUTING.md) and
[AGENTS.md](AGENTS.md) first. All five languages keep the same API; a change lands in all of them
together. Parts of this repository are generated by
[sdk-kit](https://github.com/grow-codes-packages/sdk-kit) and changed there.

## Security

Please report vulnerabilities privately to [opensource@grow.codes](mailto:opensource@grow.codes).
See [SECURITY.md](SECURITY.md).

## Licence

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for attributions.

## Trademarks and disclaimer

This is an independent, community-maintained project. It is **not affiliated with, endorsed or
sponsored by The Linux Foundation (OpenTelemetry is a CNCF project)**. OpenTelemetry Collector is a trademark of The Linux Foundation (OpenTelemetry is a CNCF project); it is used here only to name the
service this SDK connects to. The OpenTelemetry Collector server is licensed separately by its owner
(Apache-2.0).

The software is provided "AS IS", without warranties or conditions of any kind. Grow Codes is not
responsible for any damage, data loss, security incident, outage or cost that results from using it.
See [LICENSE](LICENSE) and [NOTICE](NOTICE).

## About Grow Codes

This SDK is built and maintained by [Grow Codes](https://grow.codes). Grow Codes publishes the
client libraries it builds as open source in the
[grow-codes-packages](https://github.com/grow-codes-packages) organisation, generated from one
shared kit so that every service and every language behaves the same way. Learn more at
[grow.codes](https://grow.codes).
