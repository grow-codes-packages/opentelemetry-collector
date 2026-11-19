"""One call for traces, metrics and logs over OTLP, with a shared resource.

    GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318 OTEL_SERVICE_NAME=example \
        python examples/python/telemetry.py
"""

from __future__ import annotations

import grow_opentelemetry_collector as sdk


def main() -> None:
    # Signals, endpoints, sampler, batch settings … come from the environment; the resource holds
    # service.name plus the detected host, process, container and Kubernetes attributes.
    with sdk.init_telemetry() as telemetry:
        tracer = telemetry.tracer("example")
        logger = telemetry.logger("example")
        requests = telemetry.counter("example_requests", unit="{request}")
        latency = telemetry.histogram("example_latency", unit="ms", boundaries=[10, 100, 1000])

        with sdk.with_span("handle-request", kind="server", tracer=tracer):
            requests.add(1, {"route": "/items"})
            latency.record(42.0, {"route": "/items"})
            sdk.add_event("work-done", {"items": 3})
            logger.info("request handled", {"items": 3})  # carries trace_id/span_id
            print("trace id:", sdk.current_trace_id())

        try:
            with sdk.with_span("failing-step", tracer=tracer):
                raise ValueError("boom")  # recorded on the span with ERROR status
        except ValueError:
            logger.error("step failed")

        telemetry.force_flush(10)
    # leaving the block flushes and shuts every provider down


if __name__ == "__main__":
    main()
