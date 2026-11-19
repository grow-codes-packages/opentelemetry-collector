# grow-opentelemetry-collector (Python)

Python SDK for [OpenTelemetry Collector](https://grow-codes-packages.github.io/opentelemetry-collector/) by Grow Codes.
Unofficial and community maintained; "OpenTelemetry" is a trademark of The Linux Foundation (OpenTelemetry is a CNCF project).

```bash
pip install grow-opentelemetry-collector          # OTLP over HTTP/protobuf
pip install "grow-opentelemetry-collector[grpc]"  # adds OTLP over gRPC
```

One call sets up traces, metrics and logs over OTLP (built on the official OpenTelemetry SDK):

```python
import grow_opentelemetry_collector as sdk

# GROW_OPENTELEMETRY_COLLECTOR_* / GROW_OTEL_* / OTEL_EXPORTER_OTLP_*, OTEL_SERVICE_NAME, ...
with sdk.init_telemetry() as telemetry:
    with sdk.with_span("handle-request", tracer=telemetry.tracer()):
        telemetry.counter("requests").add(1)
        telemetry.logger("app").info("handled")  # correlated with the span
```

The client checks the collector and its version:

```python
with sdk.OtelCollectorClient.from_env() as client:
    print(client.wait_until_ready(10))  # health_check :13133, zpages fallback on stock stacks
    print(client.ping(), "ms")          # empty OTLP export, authenticated with your headers
    print(client.server_version())      # "0.165.0" from zpages or target_info
```

The async client (`AsyncOtelCollectorClient`) has the same methods.

- Python 3.10 or newer, typed (`py.typed`), `httpx` for the admin endpoints, official OTLP
  exporters for traces and metrics, an OTLP log exporter for the logging category.
- Retries with full-jitter backoff and `Retry-After`, connect/request/total timeouts,
  cancellation, hooks and a silent-by-default `logging` logger named `grow_opentelemetry_collector`.
- Header credentials never appear in `repr()`, logs or error messages.
- `grow_opentelemetry_collector.testing` ships in-memory fakes and the conformance runner.

Documentation: https://grow-codes-packages.github.io/opentelemetry-collector/ ·
Source: https://github.com/grow-codes-packages/opentelemetry-collector · Licence: Apache-2.0
