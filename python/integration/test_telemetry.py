"""Integration tests: init_telemetry / init_tracing / log clients / metrics helpers export to the
real collector; data is read back from the file exporter capture and the Jaeger sidecar."""

from __future__ import annotations

import asyncio
import time
from typing import Any

import pytest
from factories import core_client
from readback import JaegerReadback, captured_logs, captured_metrics, eventually, points_with

import grow_opentelemetry_collector as sdk

PROTOCOLS = ["http/protobuf", "grpc"]


@pytest.mark.parametrize("protocol", PROTOCOLS)
def test_init_telemetry_exports_all_signals(
    env: dict[str, str], run_id: str, protocol: str
) -> None:
    tag = f"py-{protocol.replace('/', '-')}-{run_id}-{time.time_ns()}"
    service = f"it-telemetry-py-{'grpc' if protocol == 'grpc' else 'http'}"
    config = sdk.OtelCollectorConfig.from_env(
        env,
        protocol=protocol,
        service_name=service,
        service_version="1.2.3",
        environment="integration",
        resource_attributes={"grow.run": tag},
        runtime_metrics=True,
    )
    handle = sdk.init_telemetry(config)
    try:
        tracer = handle.tracer("it")
        log = handle.logger("it-logger")
        with sdk.with_span(f"root-{tag}", attributes={"tag": tag}, kind="server", tracer=tracer):
            trace_id = sdk.current_trace_id() or ""
            sdk.add_event("checkpoint", {"step": 1})
            log.warn(f"correlated {tag}", {"tag": tag})
            assert sdk.trace_fields()["trace_id"] == trace_id
            tracer.start_span("child-op").end()
            with sdk.with_span("failing-child", tracer=tracer):
                sdk.set_error(RuntimeError("boom"))
        handle.counter("it_requests", unit="{request}").add(3, {"tag": tag})
        handle.gauge("it_temperature").set(42.5, {"tag": tag})
        latency = handle.histogram("it_latency", boundaries=[10, 100], unit="ms")
        for value in (5, 50, 95):
            latency.record(value, {"tag": tag})
        sdk.counter("it_global_requests").add(1, {"tag": tag})
        sdk.up_down_counter("it_inflight").add(2, {"tag": tag})
        sdk.gauge("it_global_gauge").set(1, {"tag": tag})
        sdk.histogram("it_global_hist").record(1, {"tag": tag})
        assert sdk.meter("it-meter") is not None
        handle.force_flush()
    finally:
        handle.shutdown()
        handle.shutdown()

    # traces: Jaeger read-back (tree, kind, event, error status, resource)
    trace = JaegerReadback(env).wait_for_trace(trace_id, min_spans=3, timeout_s=20, interval_s=0.25)
    root = next(s for s in trace.spans if s.name == f"root-{tag}")
    assert root.kind == "server"
    assert root.service_name == service
    assert root.attributes["tag"] == tag
    assert root.events[0].name == "checkpoint"
    child = next(s for s in trace.spans if s.name == "child-op")
    assert child.parent_span_id == root.span_id
    failing = next(s for s in trace.spans if s.name == "failing-child")
    assert failing.status.code == "error"

    # logs: capture read-back with trace correlation and the shared resource
    found = eventually(
        lambda: next((x for x in captured_logs(env) if x.body == f"correlated {tag}"), None)
    )
    assert found.record["traceId"] == trace_id
    assert found.record["severityText"] == "WARN"
    assert found.attributes["logger.name"] == "it-logger"
    resource = found.resource
    assert resource["service.name"] == service
    assert resource["service.version"] == "1.2.3"
    assert resource["deployment.environment.name"] == "integration"
    assert resource["grow.run"] == tag
    assert isinstance(resource["host.name"], str)
    assert isinstance(resource["process.pid"], int)

    # metrics: counter (monotonic cumulative sum), gauge, histogram buckets, runtime metrics
    def metrics_for_tag() -> dict[str, Any] | None:
        mine = [m for m in captured_metrics(env) if m.resource.get("grow.run") == tag]
        names = {m.metric["name"]: m.metric for m in mine}
        return names if "it_requests" in names and "it_global_requests" in names else None

    by_name = eventually(metrics_for_tag)
    requests = by_name["it_requests"]
    assert requests["sum"]["isMonotonic"] is True
    assert requests["sum"]["aggregationTemporality"] == 2
    point = points_with(requests, "tag", tag)[0]
    assert int(point.get("asInt", point.get("asDouble", 0))) == 3
    assert points_with(by_name["it_temperature"], "tag", tag)[0]["asDouble"] == 42.5
    hist = points_with(by_name["it_latency"], "tag", tag)[0]
    assert hist["count"] == "3"
    assert hist["sum"] == 150
    assert hist["explicitBounds"] == [10, 100]
    assert hist["bucketCounts"] == ["1", "2", "0"]
    for name in (
        "it_global_requests",
        "it_inflight",
        "it_global_gauge",
        "it_global_hist",
        "process.memory.usage",
        "process.uptime",
    ):
        assert name in by_name, name


@pytest.mark.parametrize("protocol", PROTOCOLS)
def test_init_tracing_standalone(env: dict[str, str], run_id: str, protocol: str) -> None:
    name = f"py-tracing-{protocol.replace('/', '-')}-{run_id}-{time.time_ns()}"
    handle = sdk.init_tracing(
        env=env, service_name="it-tracing-py", protocol=protocol, set_global=False
    )
    span = handle.tracer("it").start_span(name)
    trace_id = format(span.get_span_context().trace_id, "032x")
    span.end()
    handle.force_flush()
    handle.shutdown()
    trace = JaegerReadback(env).wait_for_trace(trace_id, timeout_s=20, interval_s=0.25)
    assert trace.spans[0].name == name
    assert trace.spans[0].service_name == "it-tracing-py"


def test_client_owned_tracing_and_list_services(env: dict[str, str], run_id: str) -> None:
    client = core_client(env, service_name="it-client-tracing-py")
    owned = client.init_tracing(set_global=False)
    span = owned.tracer("it").start_span(f"owned-{run_id}")
    trace_id = format(span.get_span_context().trace_id, "032x")
    span.end()
    client.close()  # flushes and shuts the handle down
    jaeger = JaegerReadback(env)
    trace = jaeger.wait_for_trace(trace_id, timeout_s=20, interval_s=0.25)
    assert trace.spans[0].service_name == "it-client-tracing-py"
    assert "it-client-tracing-py" in jaeger.list_services()


def test_log_clients_batch_to_the_collector(env: dict[str, str], run_id: str) -> None:
    tag = f"py-logclient-{run_id}-{time.time_ns()}"
    client = core_client(env, service_name="it-logclient-py", protocol="grpc")
    logs = client.log_client()
    other = sdk.create_log_client(
        sdk.OtelCollectorConfig.from_env(env, service_name="it-createlogclient-py")
    )
    try:
        logs.logger("svc").child({"tag": tag}).error(f"client {tag}", {"password": "hunter2"})
        logs.send_batch([sdk.LogInput(f"batch-a {tag}"), sdk.LogInput(f"batch-b {tag}")])
        other.send(sdk.LogInput(f"other {tag}"))
        logs.flush(10)
        other.close()
        assert logs.stats().sent == 3
    finally:
        client.close()

    def all_four() -> list[Any] | None:
        mine = [x for x in captured_logs(env) if tag in x.body]
        return mine if len(mine) >= 4 else None

    found = eventually(all_four)
    error = next(x for x in found if x.body == f"client {tag}")
    assert error.attributes["password"] == "[REDACTED]"  # noqa: S105
    assert error.attributes["tag"] == tag
    assert error.resource["service.name"] == "it-logclient-py"
    other_rec = next(x for x in found if x.body == f"other {tag}")
    assert other_rec.resource["service.name"] == "it-createlogclient-py"


def test_async_log_client(env: dict[str, str], run_id: str) -> None:
    tag = f"py-async-logclient-{run_id}-{time.time_ns()}"

    async def run() -> None:
        async with sdk.AsyncOtelCollectorClient.from_env(env, service_name="it-async-py") as c:
            logs = c.log_client()
            logs.logger("a").info(f"async {tag}")
            await logs.flush(10)
            assert logs.stats().sent == 1
        standalone = sdk.create_async_log_client(sdk.OtelCollectorConfig.from_env(env))
        standalone.send(sdk.LogInput(f"standalone {tag}"))
        await standalone.close()

    asyncio.run(run())

    def both() -> list[Any] | None:
        mine = [x for x in captured_logs(env) if tag in x.body]
        return mine if len(mine) >= 2 else None

    found = eventually(both)
    assert {x.body for x in found} == {f"async {tag}", f"standalone {tag}"}
    assert next(x for x in found if x.body.startswith("async")).resource["service.name"] == (
        "it-async-py"
    )


def test_init_telemetry_against_stock_collector(env: dict[str, str], run_id: str) -> None:
    """The Grow stack configuration (built-in collector config) accepts all three signals."""
    config = sdk.OtelCollectorConfig(url=env["OTELCOL_TEST_STOCK_URL"], service_name="it-stock-py")
    client = sdk.OtelCollectorClient(config)
    handle = client.init_telemetry(set_global=False)
    handle.tracer().start_span("stock-span").end()
    handle.logger().info(f"stock-log {run_id}")
    handle.counter("stock_counter").add(1)
    handle.force_flush()
    assert handle.log_client is not None
    assert handle.log_client.stats().sent == 1
    assert handle.log_client.stats().failed == 0
    client.close()
    assert handle.is_shut_down
    detected = sdk.detect_resource_attributes()
    assert isinstance(detected["host.name"], str)
    assert "container.id" in detected  # the test runs in a container
