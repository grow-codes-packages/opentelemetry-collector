# Python examples

Install the package, point it at a running OpenTelemetry Collector and run an example:

```bash
pip install grow-opentelemetry-collector
export GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318
export OTEL_SERVICE_NAME=example
# gateway credentials, when the collector sits behind one (values are always redacted):
# export OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer%20<token>"
python examples/python/basic.py
python examples/python/telemetry.py
```

| File | Shows |
| - | - |
| `basic.py` | sync client: wait until ready, ping, server version, error handling |
| `telemetry.py` | `init_telemetry`: spans, metrics and correlated logs over OTLP |
| `async_basic.py` | async client, cancellation by task |
| `hooks_and_logging.py` | request hooks, the `grow_opentelemetry_collector` logger, retries and timeouts |
