# Trace and log queries

The OpenTelemetry Collector **stores nothing**: it receives, processes and forwards telemetry. The
category query interfaces therefore exist for API parity only and every method fails with
`UNSUPPORTED` (in every language):

| Function | Result |
| - | - |
| `client.traceQuery()` → `getTrace`, `searchTraces`, `listServices`, `listOperations`, `getDependencies`, `waitForTrace` | `UNSUPPORTED` |
| `client.logQuery()` → `search`, `tail`, `listStreams`, `listLabels`, `waitForLog` | `UNSUPPORTED` |

Query the backend the collector exports to instead, for example with the Grow Codes SDK for
[Jaeger](https://github.com/grow-codes-packages/jaeger), Grafana Tempo, Zipkin, Grafana Loki or
OpenObserve, which implement the same `TraceQueryClient` / `LogQueryClient` interfaces.

The integration tests of this repository read telemetry back through **test-only** adapters: the
collector's `file` exporter (served over HTTP by a sidecar) and a Jaeger 2.23.0 sidecar. They live in
each language's `integration/` project and are not part of the published packages.

=== "TypeScript"

    ```ts
    try {
      await client.traceQuery().getTrace(traceId);
    } catch (err) {
      // UnsupportedError, err.code === "UNSUPPORTED"
    }
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::tracing::TraceQueryClient as _;

    let err = client.trace_query().get_trace(&trace_id).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported);
    ```

=== "Go"

    ```go
    _, err := client.TraceQuery().GetTrace(ctx, traceID)
    errors.Is(err, otelcollector.ErrUnsupported) // true
    ```

=== "Python"

    ```python
    try:
        client.trace_query().get_trace(trace_id)
    except sdk.UnsupportedError:
        ...
    ```

=== "Java"

    ```java
    try {
      client.traceQuery().getTrace(traceId);
    } catch (UnsupportedException e) {
      // e.code() == ErrorCode.UNSUPPORTED
    }
    ```
