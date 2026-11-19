# One-call telemetry

`initTelemetry` sets up **traces, logs and metrics** in one call, on the official OpenTelemetry SDK
of each language, exporting OTLP to the collector. This is the generic package every Grow stack can
use: the platform deploys an OpenTelemetry Collector with every stack.

| Function | Description |
| - | - |
| `initTelemetry(config, options)` / `client.initTelemetry(options)` | tracer provider (batch span processor), OTLP log exporter + batching log client (+ an OpenTelemetry logger provider in Go and Java), meter provider (periodic reader) — all with **one resource** |
| `TelemetryHandle.tracer(name, version)` | a tracer of the handle's provider |
| `TelemetryHandle.logger(name, attributes)` | a logger whose records carry the current `trace_id` / `span_id` |
| `TelemetryHandle.meter(name)`, `counter`, `upDownCounter`, `histogram`, `gauge` | metric instruments ([Metrics](metrics.md)) |
| `TelemetryHandle.logClient`, `tracerProvider`, `meterProvider`, `resource`, `config` | the parts, for advanced use |
| `TelemetryHandle.forceFlush(timeout)` | flushes spans, log records and metrics |
| `TelemetryHandle.shutdown()` | flushes and stops everything; idempotent |

**Signals** follow the config: `traces` / `logs` / `metrics` (`GROW_OPENTELEMETRY_COLLECTOR_TRACES=false`
…), `OTEL_<SIGNAL>_EXPORTER=none` and `OTEL_SDK_DISABLED=true` turn a signal off; the options of
`initTelemetry` override that per call. `setGlobal` (default `true`) registers the providers and
propagators globally, so the [span helpers](../tracing.md) and third-party instrumentation see them.

**Resource**: detected attributes < `resourceAttributes` (`OTEL_RESOURCE_ATTRIBUTES`) < the service
fields (`service.name`, `service.version`, `deployment.environment.name`). Detectors
(`resourceDetectors`, default `host,process,container,k8s`):

| Detector | Attributes |
| - | - |
| `host` | `host.name`, `host.arch`, `os.type` |
| `process` | `process.pid`, `process.runtime.name`, `process.runtime.version`, `process.executable.name` |
| `container` | `container.id` from `/proc/self/cgroup` or `/proc/self/mountinfo` |
| `k8s` | `k8s.namespace.name`, `k8s.pod.name`, `k8s.pod.uid`, `k8s.node.name`, … from the downward-API variables (`K8S_*`, `POD_*`, `KUBERNETES_*`) |

=== "TypeScript"

    ```ts
    import { OtelCollectorClient, withSpan } from "@grow-codes/opentelemetry-collector";

    const client = OtelCollectorClient.fromEnv({ serviceName: "checkout" });
    const telemetry = client.initTelemetry();          // or initTelemetry({ config })
    const log = telemetry.logger("checkout");
    const orders = telemetry.counter("orders_total", { unit: "{order}" });

    await withSpan("checkout", async () => {
      log.info("order received", { "order.id": "o-42" }); // carries trace_id / span_id
      orders.add(1);
    });
    await telemetry.forceFlush();
    await client.close();                              // shuts the handle down
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::opentelemetry::trace::Tracer as _;
    use grow_opentelemetry_collector::logging::attrs;
    use grow_opentelemetry_collector::{init_telemetry, InstrumentOptions, OtelCollectorConfig};

    let config = OtelCollectorConfig::builder().service_name("checkout").build_from_env()?;
    let telemetry = init_telemetry(&config)?;  // or client.init_telemetry(TelemetryOptions::default())
    telemetry.tracer("checkout", None).in_span("charge-card", |_cx| {
        telemetry.logger(Some("checkout"), Default::default()).info("order received", attrs! {"order.id" => 42});
    });
    telemetry.counter("orders_total", InstrumentOptions::default().unit("{order}"))?.add(1, &[]);
    telemetry.force_flush(None).await?;
    telemetry.shutdown().await?;
    ```

=== "Go"

    ```go
    cfg, err := otelcollector.ConfigFromEnvWith(otelcollector.Config{ServiceName: "checkout"}, nil)
    if err != nil {
        return err
    }
    telemetry, err := otelcollector.InitTelemetry(ctx, cfg) // or client.InitTelemetry(ctx)
    if err != nil {
        return err
    }
    defer telemetry.Shutdown(ctx)

    orders, _ := telemetry.Counter("orders_total", otelcollector.InstrumentOptions{Unit: "{order}"})
    logger := telemetry.Logger("checkout", nil)
    _, err = otelcollector.WithSpan(ctx, "checkout", func(ctx context.Context) (struct{}, error) {
        logger.Info(ctx, "order received", "order.id", "o-42") // carries trace_id / span_id
        orders.Add(ctx, 1)
        return struct{}{}, nil
    }, otelcollector.WithTracer(telemetry.Tracer("checkout")))
    err = telemetry.ForceFlush(ctx)
    ```

=== "Python"

    ```python
    import grow_opentelemetry_collector as sdk

    config = sdk.OtelCollectorConfig.from_env(service_name="checkout")
    with sdk.init_telemetry(config) as telemetry:          # or client.init_telemetry()
        orders = telemetry.counter("orders_total", unit="{order}")
        with sdk.with_span("checkout", tracer=telemetry.tracer("checkout")):
            telemetry.logger("checkout").info("order received", {"order.id": "o-42"})
            orders.add(1)
        telemetry.force_flush(10)
    # leaving the block flushes and shuts every provider down
    ```

=== "Java"

    ```java
    try (OtelCollectorClient client = OtelCollectorClient.fromEnv()) {
      TelemetryHandle telemetry = client.initTelemetry(); // or OtelCollectorTelemetry.initTelemetry(config)
      LongCounter orders = telemetry.counter("orders_total", InstrumentOptions.unit("{order}"));
      Span span = telemetry.tracer("checkout").spanBuilder("checkout").startSpan();
      try (Scope scope = span.makeCurrent()) {
        telemetry.logger("checkout").info("order received", Map.of("order.id", "o-42"));
        orders.add(1);
      } finally {
        span.end();
      }
      telemetry.forceFlush(Duration.ofSeconds(10));
    } // closing the client shuts the handle down
    ```

## Options

| Option | Default | Meaning |
| - | - | - |
| `traces`, `logs`, `metrics` | the config's signal switches | `true`/`false`, or per-signal options (exporter factory, batch settings, extra span processors) |
| `setGlobal` | `true` | register the providers and propagators globally |
| resource detection | `resourceDetectors` of the config | detectors to run; tests inject fake environments and files |

The batch settings come from `OTEL_BSP_*` (spans) and `OTEL_BLRP_*` (log records); the metric
export interval from `OTEL_METRIC_EXPORT_INTERVAL` (default 60 s); see
[Configuration](../configuration.md).
