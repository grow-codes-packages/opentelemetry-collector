# grow-opentelemetry-collector (Rust)

Rust SDK for the [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) by
[Grow Codes](https://grow.codes): one-call telemetry (traces, logs and metrics over OTLP with the
official `opentelemetry-otlp` exporters), collector health (health_check with a zpages fallback),
ping and version. Async on tokio, with an optional blocking client.

```toml
[dependencies]
grow-opentelemetry-collector = "0.1.0"
# blocking client:   features = ["blocking"]
# test fakes/runner: features = ["testing"] (dev-dependency)
```

```rust,no_run
use grow_opentelemetry_collector::opentelemetry::trace::Tracer as _;
use grow_opentelemetry_collector::{init_telemetry, InstrumentOptions, OtelCollectorConfig};

#[tokio::main]
async fn main() -> grow_opentelemetry_collector::Result<()> {
    // explicit > GROW_OPENTELEMETRY_COLLECTOR_* > GROW_OTEL_* > OTEL_* > defaults
    let config = OtelCollectorConfig::builder().service_name("checkout").build_from_env()?;
    let telemetry = init_telemetry(&config)?; // traces + logs + metrics, one resource
    telemetry.tracer("checkout", None).in_span("charge-card", |_cx| {});
    telemetry.counter("orders", InstrumentOptions::default())?.add(1, &[]);
    telemetry.logger(Some("http"), Default::default()).info("listening", Default::default());
    telemetry.shutdown().await?;
    Ok(())
}
```

| Area | API |
| - | - |
| Config | `OtelCollectorConfig::builder()`, `from_env()`, `validate()`; OTLP `http/protobuf` (default, `:4318`) or `grpc` (`:4317`); `native` is `CONFIG` |
| Client | `OtelCollectorClient::new/from_env`, `health()`, `wait_until_ready(timeout)`, `ping()` (empty OTLP export), `server_version()` (zpages, then `target_info`), `close()` |
| Health | `CollectorHealthClient::new(&config)`, `health()`, `wait_until_ready(timeout)` |
| Telemetry | `init_telemetry(&config)`, `init_telemetry_with(&config, TelemetryOptions)`, `TelemetryHandle` (`tracer`, `meter`, `counter`, `up_down_counter`, `histogram`, `gauge`, `logger`, `log_client`, `force_flush`, `shutdown`) |
| Tracing | `init_tracing(TracingConfig)`, `tracing_config_from_env`, `collector_exporter`, plus the `tracing` category helpers |
| Logs | `OtlpLogExporter` (`LogExporter`), `create_log_client`, `client.log_client(..)`, `Logger`, `LogLayer`, `LogBridge` |
| Metrics | `meter`, `counter`, `up_down_counter`, `histogram`, `gauge`, `metric_exporter`, `create_meter_provider`, `register_runtime_metrics` |
| Resource | `detect_resource_attributes`, `telemetry_resource_attributes`, `detect_resource` (host, process, container, k8s) |
| Query | `CollectorTraceQueryClient`, `CollectorLogQueryClient`: `UNSUPPORTED` (the collector stores nothing) |

- Errors: one `Error` enum with `code()`, `http_status()`, `is_retryable()`, `retry_after()`;
  OTLP protobuf error bodies (`google.rpc.Status`) and gRPC codes are mapped; messages never
  contain credentials.
- OTLP exports use the official exporters; retryable log export failures are retried by the
  log batch processor. `ping()` and `server_version()` use the shared transport (timeouts,
  full-jitter retries, `Retry-After`, hooks); `health()` never retries.
- gRPC with `insecure_skip_verify` is `CONFIG` (tonic cannot skip verification; use
  `http/protobuf`). Log severity texts other than `TRACE`…`FATAL` become the canonical text
  (the OpenTelemetry SDK keeps `&'static str`).
- MSRV 1.88. Licence Apache-2.0. Documentation: https://grow-codes-packages.github.io/opentelemetry-collector/

Development: `make deps build lint test coverage license pack docs` (see `Makefile`).
