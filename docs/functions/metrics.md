# Metrics

Helpers on top of the OpenTelemetry metrics API. On a `TelemetryHandle` they use the handle's meter
provider; the global functions use the globally registered provider (set by `initTelemetry` with
`setGlobal`, the default).

| Function | Description |
| - | - |
| `meter(name, version)` | a meter (default name: the package name) |
| `counter(name, options)` | monotonic counter (`add`) |
| `upDownCounter(name, options)` | counter that can go down (`add` with negative values) |
| `histogram(name, options)` | histogram (`record`); `boundaries` sets explicit bucket bounds |
| `gauge(name, options)` | synchronous gauge (`record` / `set`) |
| `createMetricExporter(config)` / `metric_exporter` / `metricExporter` | the official OTLP metric exporter for the config (HTTP or gRPC, per-signal URL and headers) |
| `createMeterProvider(config, options)` | a meter provider with a periodic reader (`metricExportInterval`, default 60 s) and the config's resource |
| `registerRuntimeMetrics(meter)` | runtime metrics of the process; on by default with `runtimeMetrics: true` (`GROW_OPENTELEMETRY_COLLECTOR_RUNTIME_METRICS=true`) |

`InstrumentOptions`: `unit`, `description`, `meter` (meter name) and, for histograms, `boundaries`.
Instrument value types follow the OpenTelemetry SDK of each language (counters are integers in
Rust, Go and Java, histograms and gauges are floating point). Runtime metric **names differ per
runtime**; see [Compatibility](../compatibility.md#language-differences).

=== "TypeScript"

    ```ts
    import { counter, histogram, initTelemetry } from "@grow-codes/opentelemetry-collector";

    const telemetry = initTelemetry({ config: { serviceName: "checkout", runtimeMetrics: true } });
    const orders = telemetry.counter("orders_total", { unit: "{order}" });
    const latency = histogram("checkout_duration", { unit: "ms", boundaries: [10, 50, 100, 500] });
    const queue = telemetry.gauge("queue_depth");
    orders.add(1, { "payment.method": "card" });
    latency.record(42);
    queue.record(3);
    await telemetry.shutdown();
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::{histogram, init_telemetry, InstrumentOptions, OtelCollectorConfig};

    let config = OtelCollectorConfig::builder().runtime_metrics(true).build_from_env()?;
    let telemetry = init_telemetry(&config)?;
    telemetry.counter("orders_total", InstrumentOptions::default().unit("{order}"))?.add(1, &[]);
    histogram("checkout_duration", InstrumentOptions::default().unit("ms").boundaries(vec![10.0, 100.0]))?
        .record(42.0, &[]);
    telemetry.gauge("queue_depth", InstrumentOptions::default())?.record(3.0, &[]);
    telemetry.shutdown().await?;
    ```

=== "Go"

    ```go
    orders, err := telemetry.Counter("orders_total", otelcollector.InstrumentOptions{Unit: "{order}"})
    latency, err := otelcollector.Histogram("checkout_duration",
        otelcollector.InstrumentOptions{Unit: "ms", Boundaries: []float64{10, 50, 100, 500}})
    queue, err := telemetry.Gauge("queue_depth", otelcollector.InstrumentOptions{})
    orders.Add(ctx, 1)
    latency.Record(ctx, 42)
    queue.Record(ctx, 3)
    ```

=== "Python"

    ```python
    import grow_opentelemetry_collector as sdk

    with sdk.init_telemetry() as telemetry:
        telemetry.counter("orders_total", unit="{order}").add(1, {"payment.method": "card"})
        sdk.histogram("checkout_duration", unit="ms", boundaries=[10, 50, 100, 500]).record(42.0)
        telemetry.gauge("queue_depth").set(3)
    ```

=== "Java"

    ```java
    TelemetryHandle telemetry = OtelCollectorTelemetry.initTelemetry(OtelCollectorConfig.fromEnv());
    telemetry.counter("orders_total", InstrumentOptions.unit("{order}")).add(1);
    OtelCollectorMetrics.histogram("checkout_duration", InstrumentOptions.boundaries(10.0, 50.0, 100.0))
        .record(42.0);
    telemetry.gauge("queue_depth", InstrumentOptions.unit("1")).set(3.0);
    telemetry.shutdown();
    ```
