# grow-opentelemetry-collector (Python)

Python SDK for [OpenTelemetry Collector](https://grow-codes-packages.github.io/opentelemetry-collector/) by Grow Codes.
Unofficial and community maintained; "OpenTelemetry Collector" is a trademark of The Linux Foundation (OpenTelemetry is a CNCF project).

```bash
pip install grow-opentelemetry-collector
```

```python
from grow_opentelemetry_collector import OtelCollectorClient, OtelCollectorConfig

# Reads GROW_OPENTELEMETRY_COLLECTOR_URL, GROW_OPENTELEMETRY_COLLECTOR_TOKEN, ... from the environment.
with OtelCollectorClient.from_env() as client:
    print(client.health())
    print(client.server_version())
```

The async client has the same methods:

```python
import asyncio
from grow_opentelemetry_collector import AsyncOtelCollectorClient

async def main() -> None:
    async with AsyncOtelCollectorClient.from_env() as client:
        print(await client.health())

asyncio.run(main())
```

- Python 3.10 or newer, typed (`py.typed`), `httpx` based, environment proxies ignored unless
  configured.
- Retries with full-jitter backoff and `Retry-After`, connect/request/total timeouts,
  cancellation, hooks and a silent-by-default `logging` logger named `grow_opentelemetry_collector`.
- Credentials are wrapped in `Secret` and never appear in `repr()`, logs or error messages.
- `grow_opentelemetry_collector.testing` ships in-memory fakes and the conformance runner.

Documentation: https://grow-codes-packages.github.io/opentelemetry-collector/ ·
Source: https://github.com/grow-codes-packages/opentelemetry-collector · Licence: Apache-2.0
