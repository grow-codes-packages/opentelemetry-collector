# Logs

The [Logging category](../logging.md) bound to OTLP: an `OtlpLogExporter` that sends log records to
the collector (`POST {logsUrl}` with `application/x-protobuf`, or gRPC `LogsService/Export`) and a
batching `LogClient` on top of it with back-pressure, redaction and a logger facade.

| Function | Description |
| - | - |
| `client.logClient(options)` / `createLogClient(config, options)` | `BatchingLogClient` (`send`, `sendBatch`, `flush`, `close`, `stats`, `logger`); resource = `service.name` (+ version, environment) and `resourceAttributes`; batch settings from `OTEL_BLRP_*` |
| `client.logExporter()` / `new OtlpLogExporter(config)` | the bare exporter: `export(records, options)` → `{accepted, rejected, error}`, `shutdown()` |
| `logger(name, attributes)` | `debug` / `info` / `warn` / `error` / `fatal`; records carry the current `trace_id` / `span_id` |
| framework adapters | pino and winston (TypeScript), `tracing` layer and `log` bridge (Rust), `slog` handler and zap core (Go), `logging` handler and structlog processor (Python), Logback, Log4j2 and JUL (Java) — see [Logging](../logging.md) |

**Results and errors.** A partial success from the collector reports `rejected` > 0 with a
`VALIDATION` error. Failed exports carry the mapped SDK error (`AUTHENTICATION` for 401 /
`UNAUTHENTICATED`, `VALIDATION` for 400, `UNAVAILABLE` for network errors, …) with its natural
`retryable` flag. The official OTLP exporter has already retried 429/502/503/504 within
`exportTimeout`; the batch processor only retries what is still retryable. An exporter that was shut
down answers with `rejected` = all and an error.

**Redaction.** Attribute names such as `password`, `token`, `authorization`, `secret`, `api_key` are
replaced by `[REDACTED]` before export (configurable per client).

=== "TypeScript"

    ```ts
    import { OtelCollectorClient } from "@grow-codes/opentelemetry-collector";

    const client = OtelCollectorClient.fromEnv({ serviceName: "checkout" });
    const logs = client.logClient();
    logs.send({ message: "order received", attributes: { "order.id": "o-42" } });
    logs.logger("http").warn("slow request", { "http.route": "/pay", password: "redacted" });
    await logs.flush();
    console.log(logs.stats()); // { sent, dropped, failed, queued, ... }

    const exporter = client.logExporter();
    const result = await exporter.export([], { gzip: true }); // { accepted, rejected, error? }
    await client.close(); // flushes and closes both
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::logging::{attrs, LogClient as _};
    use grow_opentelemetry_collector::{LogClientOptions, OtelCollectorClient};

    let client = OtelCollectorClient::from_env()?;
    let logs = client.log_client(LogClientOptions::default())?;
    logs.send("order received".into());
    logs.logger(Some("http"), attrs! {"component" => "pay"}).warn("slow request", attrs! {"http.route" => "/pay"});
    logs.flush(None).await?;
    println!("{:?}", logs.stats());
    let exporter = client.log_exporter()?; // OtlpLogExporter (LogExporter)
    client.close().await;                  // flushes and closes both
    ```

=== "Go"

    ```go
    logs, err := client.LogClient(otelcollector.LogClientOptions{})
    if err != nil {
        return err
    }
    logs.Send(ctx, otelcollector.LogInput{Message: "order received",
        Attributes: map[string]otelcollector.AttrValue{"order.id": "o-42"}})
    logs.Logger("http", nil).Warn(ctx, "slow request", "http.route", "/pay")
    slog.New(otelcollector.NewSlogHandler(logs, nil)).InfoContext(ctx, "via slog")
    err = logs.Flush(ctx)
    fmt.Println(logs.Stats().Sent)

    exporter, err := client.LogExporter()
    res, err := exporter.Export(ctx, []otelcollector.LogRecord{{Body: "raw record"}}, otelcollector.ExportOptions{Gzip: true})
    ```

=== "Python"

    ```python
    import grow_opentelemetry_collector as sdk

    with sdk.OtelCollectorClient.from_env() as client:
        logs = client.log_client()
        logs.send(sdk.LogInput("order received", attributes={"order.id": "o-42"}))
        logs.logger("http").warn("slow request", {"http.route": "/pay"})
        logs.flush()
        print(logs.stats())

        exporter = client.log_exporter()
        result = exporter.export([sdk.LogRecord("raw record")])  # ExportResult(accepted, rejected, error)
    # asyncio: AsyncOtelCollectorClient.log_client() / create_async_log_client(config)
    ```

=== "Java"

    ```java
    try (OtelCollectorClient client = OtelCollectorClient.fromEnv()) {
      BatchingLogClient logs = client.logClient();
      logs.send(LogInput.of("order received"));
      logs.logger("http").warn("slow request", Map.of("http.route", "/pay"));
      logs.flush();
      System.out.println(logs.stats());

      OtlpLogExporter exporter = client.logExporter();
    } // closing the client flushes and closes both
    ```
