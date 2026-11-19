# Client and health

`OtelCollectorClient` checks a collector and owns the telemetry handles, log clients and exporters
made through it: `close()` flushes and shuts all of them down. Build it from the environment or an
explicit config; it is safe to share between threads/tasks.

| Function | Request | Result |
| - | - | - |
| `fromEnv()` / constructor | — | client; `CONFIG` for an invalid setting (the message names the variable, never its value) |
| `health()` | `GET {healthUrl}` (`health_check`, port 13133), else `GET {zpagesUrl}/debug/servicez` (port 55679) | `HealthStatus` (`healthy`, `status`, `details.source` = `health_check` or `zpages`); never throws for an unhealthy or unreachable collector |
| `waitUntilReady(timeout)` | polls `health()` every 500 ms | the healthy status; `TIMEOUT` after `timeout` (default 30 s) |
| `ping()` | empty OTLP trace export (`POST {tracesUrl}` or gRPC `TraceService/Export`) with the configured headers | latency; `AUTHENTICATION` / `PERMISSION_DENIED` when an authenticator or gateway rejects the headers, `NOT_FOUND` for a wrong path |
| `serverVersion()` | zpages `/debug/servicez` "Version" row, else `target_info` at `telemetryUrl` | e.g. `0.165.0`; `UNSUPPORTED` when neither answers |
| `close()` | — | idempotent; flushes and shuts down everything the client created; later calls fail with `CANCELLED` |
| `CollectorHealthClient` | same as `health()` / `waitUntilReady()` / `serverVersion()` | standalone health client (also `client.healthClient` / `collectorHealth()`) |

The admin endpoints are **unauthenticated**: the configured export headers are never sent to
them. `health()` does not retry; `ping()` and `serverVersion()` use the shared transport
(timeouts, full-jitter retries, `Retry-After`, hooks).

!!! note "Stock collectors"
    The built-in configuration of `otel/opentelemetry-collector-contrib` binds `health_check` to
    `localhost`, so it cannot be reached from another container. `health()` then answers from zpages
    (`details.source = "zpages"`). `serverVersion()` reads zpages as well, so it works on stock stacks.

=== "TypeScript"

    ```ts
    import { OtelCollectorClient } from "@grow-codes/opentelemetry-collector";

    const client = OtelCollectorClient.fromEnv(); // or new OtelCollectorClient({ url: "http://otel:4318" })
    try {
      const health = await client.waitUntilReady(10_000);  // { healthy, status, details: { source } }
      const ms = await client.ping({ signal: AbortSignal.timeout(5_000) });
      const version = await client.serverVersion();        // "0.165.0"
    } finally {
      await client.close();
    }
    ```

=== "Rust"

    ```rust
    use std::time::Duration;
    use grow_opentelemetry_collector::{OtelCollectorClient, OtelCollectorConfig};

    let config = OtelCollectorConfig::builder().url("http://otel:4318").build_from_env()?;
    let client = OtelCollectorClient::new(config)?;
    let health = client.wait_until_ready(Duration::from_secs(10)).await?;
    let latency = client.ping().await?;
    let version = client.server_version().await?; // "0.165.0"
    client.close().await;
    ```

=== "Go"

    ```go
    client, err := otelcollector.NewClientFromEnv()
    if err != nil {
        return err
    }
    defer client.Close()

    health, err := client.WaitUntilReady(ctx, 10*time.Second)
    latency, err := client.Ping(ctx)
    version, err := client.ServerVersion(ctx) // "0.165.0"
    ```

=== "Python"

    ```python
    from grow_opentelemetry_collector import OtelCollectorClient

    with OtelCollectorClient.from_env() as client:
        health = client.wait_until_ready(10)  # seconds
        ms = client.ping()
        version = client.server_version()     # "0.165.0"

    # asyncio: AsyncOtelCollectorClient has the same methods (async with, await client.ping())
    ```

=== "Java"

    ```java
    import codes.grow.packages.otelcollector.OtelCollectorClient;

    try (OtelCollectorClient client = OtelCollectorClient.fromEnv()) {
      HealthStatus health = client.waitUntilReady(Duration.ofSeconds(10));
      Duration latency = client.ping();
      String version = client.serverVersion(); // "0.165.0"
      // every I/O call also has an …Async variant returning a CompletableFuture
    }
    ```

## Standalone health client

=== "TypeScript"

    ```ts
    import { CollectorHealthClient } from "@grow-codes/opentelemetry-collector";

    const health = new CollectorHealthClient({ url: "http://otel:4318" });
    console.log((await health.health()).details.source); // "health_check" or "zpages"
    await health.close();
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::{CollectorHealthClient, OtelCollectorConfig};

    let health = CollectorHealthClient::new(&OtelCollectorConfig::builder().url("http://otel:4318").build()?)?;
    let status = health.health().await?;
    health.close();
    ```

=== "Go"

    ```go
    h, err := otelcollector.NewCollectorHealthClient(otelcollector.Config{
        CommonConfig: otelcollector.CommonConfig{URL: "http://otel:4318"},
    })
    if err != nil {
        return err
    }
    defer h.Close()
    status, err := h.Health(ctx)
    ```

=== "Python"

    ```python
    from grow_opentelemetry_collector import CollectorHealthClient, OtelCollectorConfig

    health = CollectorHealthClient(OtelCollectorConfig(url="http://otel:4318"))
    print(health.health().details["source"])
    health.close()
    ```

=== "Java"

    ```java
    CollectorHealthClient health =
        CollectorHealthClient.create(OtelCollectorConfig.builder().url("http://otel:4318").build());
    HealthStatus status = health.health();
    ```

## Errors on a closed client

Every call on a closed client — `health()`, `ping()`, `serverVersion()`, `waitUntilReady()` and the
factories `initTelemetry()`, `initTracing()`, `logClient()`, `logExporter()` — fails with
`CANCELLED` in every language.
