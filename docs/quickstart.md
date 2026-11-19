# Quickstart

Check that the collector is up, then send traces, logs and metrics with one call and close
everything cleanly. The packages read the standard OpenTelemetry variables, so code that already
runs with `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_SERVICE_NAME` works unchanged; the
`GROW_OPENTELEMETRY_COLLECTOR_*` variables (alias `GROW_OTEL_*`) take precedence
([Configuration](configuration.md)).

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318   # or GROW_OPENTELEMETRY_COLLECTOR_URL
export OTEL_SERVICE_NAME=checkout
# export OTEL_EXPORTER_OTLP_PROTOCOL=grpc                  # port 4317
# export OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer%20<token>"   # only behind an authenticator
```

=== "TypeScript"

    ```ts
    import { OtelCollectorClient, withSpan } from "@grow-codes/opentelemetry-collector";

    const client = OtelCollectorClient.fromEnv();
    try {
      const health = await client.waitUntilReady(10_000);
      console.log("healthy:", health.healthy, "via", health.details.source);
      console.log("ping:", await client.ping(), "ms");

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

=== "Rust"

    ```rust
    use std::time::Duration;
    use grow_opentelemetry_collector::opentelemetry::trace::Tracer as _;
    use grow_opentelemetry_collector::{InstrumentOptions, OtelCollectorClient, TelemetryOptions};

    #[tokio::main]
    async fn main() -> grow_opentelemetry_collector::Result<()> {
        let client = OtelCollectorClient::from_env()?;
        let health = client.wait_until_ready(Duration::from_secs(10)).await?;
        println!("healthy: {} ({})", health.healthy, health.status);
        println!("ping: {:?}", client.ping().await?);

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

=== "Go"

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

=== "Python"

    ```python
    import grow_opentelemetry_collector as sdk

    with sdk.OtelCollectorClient.from_env() as client:
        health = client.wait_until_ready(10)
        print("healthy:", health.healthy, "via", health.details["source"])
        print("ping:", client.ping(), "ms")

        telemetry = client.init_telemetry()  # traces + logs + metrics over OTLP
        orders = telemetry.counter("orders_total")
        with sdk.with_span("checkout", tracer=telemetry.tracer("checkout")):
            telemetry.logger("checkout").info("order received", {"order.id": "o-42"})
            orders.add(1)
    # leaving the block flushes and shuts the telemetry down
    ```

    Async code uses `AsyncOtelCollectorClient` with the same methods (`async with`, `await client.ping()`).

=== "Java"

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
      System.out.println("healthy: " + health.healthy() + " via " + health.details().get("source"));
      System.out.println("ping: " + client.ping().toMillis() + " ms");

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

## Next steps

- [Functions](functions.md): every function with an example in all five languages.
- [Configuration](configuration.md): every setting and environment variable.
- [Tracing](tracing.md): span helpers, propagation and framework middleware.
- [Logging](logging.md): the log client, logger facade and framework adapters.
- [Error handling](errors.md) and [Troubleshooting](troubleshooting.md).
- Runnable programs: [`examples/`](https://github.com/grow-codes-packages/opentelemetry-collector/tree/main/examples).
