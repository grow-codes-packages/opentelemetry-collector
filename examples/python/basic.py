"""Health, ping and server version with the synchronous client."""

from __future__ import annotations

import grow_opentelemetry_collector as sdk


def main() -> None:
    # Reads GROW_OPENTELEMETRY_COLLECTOR_*, GROW_OTEL_* and the standard OTEL_EXPORTER_OTLP_*
    # variables. Gateway credentials go in headers (OTEL_EXPORTER_OTLP_HEADERS), never logged.
    with sdk.OtelCollectorClient.from_env() as client:
        status = client.wait_until_ready(10)
        print(f"healthy={status.healthy} status={status.status} source={status.details['source']}")
        try:
            print(f"ping: {client.ping():.1f} ms")
            print(f"collector version: {client.server_version()}")
        except sdk.AuthenticationError as err:
            print(f"the collector rejected the configured headers: {err}")
        except sdk.UnsupportedError:
            print("this collector exposes no version (zpages and internal telemetry closed)")
        except sdk.OtelCollectorError as err:
            print(f"failed with {err.code}: {err} (retryable={err.retryable})")


if __name__ == "__main__":
    main()
