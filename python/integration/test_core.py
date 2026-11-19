"""Integration tests against the real collector (docker/compose.yml): client surface, health (incl.
the stock collector's loopback-only health_check), ping over HTTP and gRPC (with the
bearertokenauth receiver), server_version, the OTLP log exporters, the query clients and the core
conformance scenarios."""

from __future__ import annotations

import asyncio
import time
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urlsplit

import pytest
from factories import CORE_CAPABILITIES, async_core_client, core_client
from readback import captured_logs, eventually

import grow_opentelemetry_collector as sdk
from grow_opentelemetry_collector.testing import ConformanceRunner, assert_scenario, pytest_params

TRACE_ID = "4bf92f3577b34da6a3ce929d0e0e4736"
SPAN_ID = "00f067aa0ba902b7"
PROTOCOLS = ["http/protobuf", "grpc"]


def test_from_env_reads_the_bootstrap_variables(env: dict[str, str]) -> None:
    with core_client(env) as c:
        assert c.config.url == env["GROW_OPENTELEMETRY_COLLECTOR_URL"]
        assert c.config.grpc_url == env["GROW_OPENTELEMETRY_COLLECTOR_GRPC_URL"]
        assert c.config.protocol == "http/protobuf"
        assert c.config.telemetry_url == env["GROW_OPENTELEMETRY_COLLECTOR_TELEMETRY_URL"]


def test_health_and_wait_until_ready(client: sdk.OtelCollectorClient) -> None:
    status = client.health()
    assert status.healthy, status
    assert status.status == "ok"
    assert status.details["source"] == "health_check"
    assert status.details["vendorStatus"] == "Server available"
    assert status.details["upSince"][:4].isdigit()
    assert client.wait_until_ready(10).healthy
    assert client.health_client.health().healthy


def test_stock_collector_health_falls_back_to_zpages(env: dict[str, str]) -> None:
    stock = env["OTELCOL_TEST_STOCK_URL"]
    host = urlsplit(stock).hostname
    cfg = sdk.OtelCollectorConfig(url=stock, retry=sdk.RetryPolicy(max_retries=0))
    with sdk.CollectorHealthClient(cfg) as h:
        assert h.health() == sdk.HealthStatus(True, "ok", {"source": "zpages"})
        assert h.server_version() == env["OTELCOL_TEST_SERVER_VERSION"]
        assert h.wait_until_ready(5).healthy
    dead = sdk.OtelCollectorConfig(
        url=stock, zpages_url=f"http://{host}:1", retry=sdk.RetryPolicy(max_retries=0)
    )
    with sdk.CollectorHealthClient(dead) as h:
        status = h.health()
        assert (status.healthy, status.status) == (False, "unreachable")


@pytest.mark.parametrize("protocol", PROTOCOLS)
def test_ping(env: dict[str, str], protocol: str) -> None:
    with core_client(env, protocol=protocol) as c:
        latency = c.ping()
        assert isinstance(latency, float)
        assert latency >= 0


@pytest.mark.parametrize("protocol", PROTOCOLS)
def test_ping_bearer_token_receiver(env: dict[str, str], protocol: str) -> None:
    token = env["OTELCOL_TEST_BEARER_TOKEN"]
    wrong = "wrong-token-for-integration"
    target: dict[str, Any] = (
        {"grpc_url": env["OTELCOL_TEST_AUTH_GRPC_URL"]}
        if protocol == "grpc"
        else {"url": env["OTELCOL_TEST_AUTH_URL"]}
    )
    good = sdk.OtelCollectorConfig(
        protocol=protocol, headers={"Authorization": f"Bearer {token}"}, **target
    )
    with sdk.OtelCollectorClient(good) as c:
        assert c.ping() >= 0
    bad = sdk.OtelCollectorConfig(
        protocol=protocol, headers={"Authorization": f"Bearer {wrong}"}, **target
    )
    with sdk.OtelCollectorClient(bad) as c, pytest.raises(sdk.AuthenticationError) as info:
        c.ping()
    assert info.value.code == sdk.ErrorCode.AUTHENTICATION
    assert wrong not in str(info.value)
    assert token not in str(info.value)
    assert wrong not in repr(bad)


def test_server_version_zpages_and_target_info(env: dict[str, str]) -> None:
    want = env["OTELCOL_TEST_SERVER_VERSION"]
    c = core_client(env)
    assert c.server_version() == want
    c.close()
    c.close()
    host = urlsplit(env["GROW_OPENTELEMETRY_COLLECTOR_URL"]).hostname
    # zpages pointed at pprof (404 for /debug/servicez): falls back to target_info on :8888
    with core_client(env, zpages_url=f"http://{host}:1777") as fallback:
        assert fallback.server_version() == want
    with (
        sdk.OtelCollectorClient(
            sdk.OtelCollectorConfig(
                url=env["GROW_OPENTELEMETRY_COLLECTOR_URL"], zpages_url=f"http://{host}:1777"
            )
        ) as none,
        pytest.raises(sdk.UnsupportedError),
    ):
        none.server_version()


def test_close_is_idempotent(env: dict[str, str]) -> None:
    c = core_client(env)
    c.close()
    c.close()
    with pytest.raises(sdk.CancelledError):
        c.ping()


def _record(marker: str, run: str) -> sdk.LogRecord:
    return sdk.LogRecord(
        marker,
        severity=sdk.Severity.ERROR,
        timestamp=datetime.now(timezone.utc),
        attributes={"run": run, "n": 7, "nested": {"ok": True}},
        resource={"service.name": "it-exporter-py"},
        trace_id=TRACE_ID,
        span_id=SPAN_ID,
    )


@pytest.mark.parametrize("protocol", PROTOCOLS)
def test_log_exporter_round_trip(env: dict[str, str], run_id: str, protocol: str) -> None:
    marker = f"py-exporter-{protocol.replace('/', '-')}-{run_id}-{time.time_ns()}"
    with core_client(env, protocol=protocol) as c:
        exporter = c.log_exporter()
        result = exporter.export([_record(marker, run_id)], gzip=True, timeout_s=10)
        assert (result.accepted, result.rejected, result.error) == (1, 0, None)
    found = eventually(lambda: next((x for x in captured_logs(env) if x.body == marker), None))
    assert found.resource["service.name"] == "it-exporter-py"
    assert found.attributes["n"] == 7
    assert found.attributes["nested"] == {"ok": True}
    assert found.record["severityNumber"] == 17
    assert found.record["severityText"] == "ERROR"
    assert found.record["traceId"] == TRACE_ID
    assert found.record["spanId"] == SPAN_ID
    assert found.scope == sdk.LOG_SCOPE_NAME


def test_log_exporter_auth_failure(env: dict[str, str]) -> None:
    denied = sdk.OtlpLogExporter(sdk.OtelCollectorConfig(url=env["OTELCOL_TEST_AUTH_URL"]))
    result = denied.export([_record("denied", "x")], gzip=False)
    assert result.accepted == 0
    assert isinstance(result.error, sdk.AuthenticationError)
    denied.shutdown()


def test_async_client_and_exporter(env: dict[str, str], run_id: str) -> None:
    marker = f"py-async-exporter-{run_id}-{time.time_ns()}"

    async def run() -> None:
        async with async_core_client(env) as c:
            assert (await c.health()).healthy
            assert await c.ping() >= 0
            assert await c.server_version() == env["OTELCOL_TEST_SERVER_VERSION"]
            assert (await c.wait_until_ready(5)).healthy
            exporter = c.log_exporter()
            assert (await exporter.export([_record(marker, run_id)])).accepted == 1
        async with async_core_client(env, protocol="grpc") as g:
            assert await g.ping() >= 0
            exporter = g.log_exporter()
            assert (await exporter.export([_record(marker + "-grpc", run_id)])).accepted == 1
            with pytest.raises(sdk.UnsupportedError):
                await g.trace_query().get_trace(TRACE_ID)
            with pytest.raises(sdk.UnsupportedError):
                await g.log_query().search("x")

    asyncio.run(run())
    wanted = {marker, marker + "-grpc"}

    def both() -> set[str] | None:
        bodies = {x.body for x in captured_logs(env)} & wanted
        return bodies if bodies == wanted else None

    assert eventually(both) == wanted


def test_query_clients_are_unsupported(client: sdk.OtelCollectorClient) -> None:
    with pytest.raises(sdk.UnsupportedError):
        client.trace_query().get_trace(TRACE_ID)
    with pytest.raises(sdk.UnsupportedError):
        client.trace_query().list_services()
    with pytest.raises(sdk.UnsupportedError):
        client.log_query().search("x")


@pytest.mark.parametrize("scenario", pytest_params(categories=["core"]))
def test_core_conformance(env: dict[str, str], scenario: dict[str, Any]) -> None:
    runner = ConformanceRunner(
        {"core": lambda ctx: core_client(env)}, capabilities=CORE_CAPABILITIES
    )
    assert_scenario(runner, scenario)


def test_core_conformance_async(env: dict[str, str]) -> None:
    runner = ConformanceRunner(
        {"core": lambda ctx: async_core_client(env)}, capabilities=CORE_CAPABILITIES
    )
    report = runner.run(runner.scenarios())
    assert report.ok, report.summary()
