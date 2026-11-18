# Python examples

Install the package, point it at a running OpenTelemetry Collector and run an example:

```bash
pip install grow-opentelemetry-collector
export GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318
export GROW_OPENTELEMETRY_COLLECTOR_TOKEN=...   # when the service needs one
python examples/python/basic.py
```

| File | Shows |
| - | - |
| `basic.py` | sync client: health, ping, server version, error handling |
| `async_basic.py` | async client, cancellation by task |
| `hooks_and_logging.py` | request hooks, the `grow_opentelemetry_collector` logger, retries and timeouts |
