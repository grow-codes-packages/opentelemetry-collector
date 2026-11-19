# Tracing setup

`initTracing` is the [Tracing category](../tracing.md) setup with collector defaults: OTLP to the
collector, service name, resource attributes, sampler, propagators and batch settings from the
collector config (`GROW_OPENTELEMETRY_COLLECTOR_*`, `GROW_OTEL_*`, `OTEL_*`). Fields set on the
`TracingConfig` win over those defaults. Use it when you only want traces; `initTelemetry` sets up all
three signals.

| Function | Description |
| - | - |
| `initTracing(tracingConfig, {collector})` / `client.initTracing(tracingConfig)` | `TracingHandle` (`tracerProvider`, `tracer(name, version)`, `forceFlush()`, `shutdown()`, `setGlobal`) with the official OTLP span exporter |
| `tracingConfigFrom(collectorConfig, tracingConfig)` | the effective `TracingConfig` (no side effects) |
| `resolveTracesEndpoint(protocol, endpoint)` | HTTP: a URL without a path gets `/v1/traces`; gRPC: used as is |
| span exporter factory (`collectorSpanExporterFactory` / `collector_exporter` / `NewExporter` / `createSpanExporter`) | the OTLP span exporter for a resolved config, for custom pipelines |

**Endpoint and protocol**

| Protocol | Default endpoint | Explicit endpoint |
| - | - | - |
| `http/protobuf` (default) | `url` + `/v1/traces` (`http://127.0.0.1:4318/v1/traces`) | `tracesUrl` / `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, used as is |
| `grpc` | `grpcUrl` (default: scheme and host of `url`, port 4317) | `tracesUrl` / `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (the per-signal URL wins in gRPC mode too) |
| `native` | — | `CONFIG`: the collector has no native protocol |

`https://` enables TLS (CA bundle and client certificate from `tls.*`); headers
(`OTEL_EXPORTER_OTLP_HEADERS`, per-signal `OTEL_EXPORTER_OTLP_TRACES_HEADERS`) and gzip compression
apply to every export. Retries on 429/502/503/504 (and the retryable gRPC codes) are done by the
official exporters within `exportTimeout` (default 10 s); see the Rust note in
[Compatibility](../compatibility.md#language-differences).

=== "TypeScript"

    ```ts
    import { initTracing, withSpan } from "@grow-codes/opentelemetry-collector";

    const tracing = initTracing({ serviceName: "checkout", protocol: "grpc" });
    console.log(tracing.endpoint); // "http://127.0.0.1:4317"
    await withSpan("charge-card", async (span) => {
      span.setAttribute("order.id", "o-42");
    });
    await tracing.shutdown();
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::tracing::{with_span, SpanOptions, TracingConfig};
    use grow_opentelemetry_collector::{init_tracing, Error};

    let tracing = init_tracing(TracingConfig::new("checkout"))?; // or client.init_tracing(...)
    with_span("charge-card", SpanOptions::default(), async { Ok::<_, Error>(()) }).await?;
    tracing.shutdown().await?;
    ```

=== "Go"

    ```go
    tracing, err := otelcollector.InitTracing(ctx, otelcollector.TracingConfig{ServiceName: "checkout"})
    // or client.InitTracing(ctx, otelcollector.TracingConfig{}) with the client's config as defaults
    if err != nil {
        return err
    }
    defer tracing.Shutdown(ctx)
    _, err = otelcollector.WithSpan(ctx, "charge-card", func(ctx context.Context) (int, error) {
        return 42, nil
    })
    ```

=== "Python"

    ```python
    import grow_opentelemetry_collector as sdk

    tracing = sdk.init_tracing(service_name="checkout", protocol="grpc")  # needs the [grpc] extra
    with sdk.with_span("charge-card", attributes={"order.id": "o-42"}):
        pass
    tracing.shutdown()
    ```

=== "Java"

    ```java
    TracingHandle tracing =
        OtelCollectorTracing.initTracing(TracingConfig.builder("checkout").build());
    Span span = tracing.tracer("checkout").spanBuilder("charge-card").startSpan();
    span.end();
    tracing.shutdown();
    ```

The span helpers (`withSpan`, `traced`, `currentTraceId`, `addEvent`, `recordException`,
`setError`), propagation (`inject`/`extract`), log correlation (`traceFields`) and the framework
adapters are documented on the [Tracing](../tracing.md) page.
