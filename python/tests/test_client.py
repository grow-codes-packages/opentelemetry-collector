"""OtelCollectorClient / AsyncOtelCollectorClient: health (health_check + zpages fallback), ping
over HTTP and gRPC, server_version, owned resources, close and the unsupported query clients.
HTTP replies replay the recorded collector 0.165.0 fixtures."""

from __future__ import annotations

import asyncio
import threading
import time
from typing import Any

import grpc
import pytest

import grow_opentelemetry_collector as sdk
from _grpc import GrpcReply, GrpcServer
from _support import MockServer, Reply, closed_port_url, fixture_reply, make_config

TOKEN = "client-token-789"


def config(server: MockServer, **kw: Any) -> sdk.OtelCollectorConfig:
    kw.setdefault("health_url", server.url + "/")
    kw.setdefault("zpages_url", server.url)
    return make_config(server.url, **kw)


def client(server: MockServer, **kw: Any) -> sdk.OtelCollectorClient:
    return sdk.OtelCollectorClient(config(server, **kw))


def aclient(server: MockServer, **kw: Any) -> sdk.AsyncOtelCollectorClient:
    return sdk.AsyncOtelCollectorClient(config(server, **kw))


HEALTH_OK = sdk.HealthStatus(
    True,
    "ok",
    {
        "source": "health_check",
        "vendorStatus": "Server available",
        "upSince": "2026-11-18T20:53:24.720766521Z",
        "uptime": "8.849963868s",
    },
)


# --------------------------------------------------------------------------- health


def test_health_ok_without_credentials(server: MockServer) -> None:
    server.enqueue("/", fixture_reply("health.ok"))
    with client(server, token=TOKEN) as c:
        assert c.health() == HEALTH_OK
        assert isinstance(c.health_client, sdk.CollectorHealthClient)
        assert TOKEN not in repr(c)
    assert "Authorization" not in server.seen[0].headers


def test_health_unhealthy_and_plain_bodies(server: MockServer) -> None:
    c = client(server)
    server.enqueue("/", Reply(503, {"status": "Server not available", "upSince": "x"}))
    assert c.health() == sdk.HealthStatus(
        False,
        "unhealthy",
        {
            "source": "health_check",
            "vendorStatus": "Server not available",
            "upSince": "x",
            "httpStatus": "503",
        },
    )
    server.enqueue("/", Reply(200, "OK"))
    assert c.health() == sdk.HealthStatus(True, "ok", {"source": "health_check"})
    server.enqueue("/", Reply(500, [1]))
    assert c.health().status == "unhealthy"
    assert len(server.seen) == 3  # health never retries
    c.close()


def test_health_falls_back_to_zpages(server: MockServer) -> None:
    """Stock stacks bind health_check to localhost: health() answers from zpages."""
    server.enqueue("/debug/servicez", fixture_reply("serverVersion.zpages_stock"))
    c = sdk.OtelCollectorClient(
        make_config(server.url, health_url=closed_port_url() + "/", zpages_url=server.url)
    )
    assert c.health() == sdk.HealthStatus(True, "ok", {"source": "zpages"})
    server.enqueue("/debug/servicez", Reply(404, "404 page not found"))
    status = c.health()
    assert (status.healthy, status.status) == (False, "unreachable")
    assert "connection failed" in status.details["error"]
    c.close()


def test_core_health_unreachable() -> None:
    """core.health.unreachable"""
    c = sdk.OtelCollectorClient(make_config(closed_port_url()))
    status = c.health()
    assert not status.healthy and status.status == "unreachable"
    c.close()


def test_health_raises_cancelled(server: MockServer) -> None:
    token = sdk.CancelToken()
    token.cancel()
    with client(server) as c, pytest.raises(sdk.CancelledError):
        c.health(cancel=token)


def test_default_health_and_zpages_urls() -> None:
    cfg = sdk.OtelCollectorConfig(url="https://collector.example:4318")
    assert cfg.health_url == "https://collector.example:13133/"
    assert cfg.zpages_url == "https://collector.example:55679"
    assert cfg.grpc_url == "https://collector.example:4317"
    v6 = sdk.OtelCollectorConfig(url="http://[::1]:4318")
    assert v6.grpc_url == "http://[::1]:4317"


def test_wait_until_ready(server: MockServer) -> None:
    server.enqueue("/", Reply(503, {"status": "Server not available"}), fixture_reply("health.ok"))
    with client(server) as c:
        assert c.wait_until_ready(5, interval_s=0.01) == HEALTH_OK
        server.defaults["/"] = Reply(503, {})
        with pytest.raises(sdk.TimeoutError):
            c.wait_until_ready(0.05, interval_s=0.01)
        token = sdk.CancelToken()
        threading.Timer(0.05, token.cancel).start()
        started = time.monotonic()
        with pytest.raises(sdk.CancelledError):
            c.wait_until_ready(10, interval_s=5, cancel=token)
        assert time.monotonic() - started < 3
    h = sdk.CollectorHealthClient(config(server))
    with pytest.raises(sdk.TimeoutError):
        h.wait_until_ready(0.02, interval_s=0.01)
    h.close()


# --------------------------------------------------------------------------- ping


def test_ping_http_empty_export(server: MockServer) -> None:
    server.enqueue("/v1/traces", fixture_reply("ping.http_empty_export"))
    with client(server, token=TOKEN, traces_headers={"X-Tenant": "t1"}) as c:
        latency = c.ping()
        assert isinstance(latency, float) and latency >= 0
    seen = server.seen[0]
    assert (seen.method, seen.path, seen.body) == ("POST", "/v1/traces", b"")
    assert seen.headers["Content-Type"] == "application/x-protobuf"
    assert seen.headers["Authorization"] == f"Bearer {TOKEN}"
    assert seen.headers["X-Tenant"] == "t1"
    assert seen.headers["User-Agent"].startswith("grow-codes-opentelemetry-collector/")


def test_ping_explicit_traces_url(server: MockServer) -> None:
    server.enqueue("/custom/path", Reply(200, b"\n\x00"))
    with client(server, traces_url=server.url + "/custom/path") as c:
        c.ping()
    assert server.seen[0].path == "/custom/path"


def test_ping_errors(server: MockServer) -> None:
    server.enqueue("/v1/traces", fixture_reply("ping.http_unauthenticated_wrong_token"))
    with client(server, token=TOKEN) as c:
        with pytest.raises(sdk.AuthenticationError) as info:
            c.ping()
        assert "provided authorization does not match" in info.value.message
        assert TOKEN not in str(info.value)
        assert info.value.http_status == 401
        server.enqueue("/v1/traces", fixture_reply("export.http_not_found"))
        with pytest.raises(sdk.NotFoundError) as nf:
            c.ping()
        assert "404 page not found" in nf.value.message
        server.enqueue("/v1/traces", Reply(403, b""))
        with pytest.raises(sdk.PermissionDeniedError):
            c.ping()
        server.enqueue("/v1/traces", Reply(503, b""), Reply(200, b"\n\x00"))
        c.ping()  # the empty export is idempotent: retried


def test_ping_grpc(grpc_server: GrpcServer, server: MockServer) -> None:
    with client(server, protocol="grpc", grpc_url=grpc_server.url, token=TOKEN) as c:
        assert c.ping() >= 0
        assert c.ping() >= 0  # channel reused
        seen = grpc_server.seen[0]
        assert (seen.method, seen.body) == ("traces", b"")
        assert seen.metadata["authorization"] == f"Bearer {TOKEN}"
        assert "grow-codes-opentelemetry-collector" in seen.metadata["user-agent"]
        grpc_server.enqueue(
            "traces",
            GrpcReply(code=grpc.StatusCode.UNAUTHENTICATED, details=f"bad token {TOKEN}"),
        )
        with pytest.raises(sdk.AuthenticationError) as info:
            c.ping()
        assert "UNAUTHENTICATED" in info.value.message and TOKEN not in info.value.message
        grpc_server.enqueue("traces", GrpcReply(code=grpc.StatusCode.UNAVAILABLE), GrpcReply())
        c.ping()
        grpc_server.enqueue("traces", GrpcReply(code=grpc.StatusCode.UNIMPLEMENTED, details="no"))
        with pytest.raises(sdk.NotFoundError):
            c.ping()


def test_ping_grpc_timeout_and_cancel(grpc_server: GrpcServer, server: MockServer) -> None:
    with client(server, protocol="grpc", grpc_url=grpc_server.url, request_timeout=0.2) as c:
        grpc_server.enqueue("traces", GrpcReply(delay=1.0))
        with pytest.raises(sdk.TimeoutError):
            c.ping()
        grpc_server.enqueue("traces", GrpcReply(delay=2.0))
        token = sdk.CancelToken()
        threading.Timer(0.05, token.cancel).start()
        with pytest.raises(sdk.CancelledError):
            c.ping(cancel=token)
        with pytest.raises(sdk.CancelledError):
            c.ping(cancel=token)  # already cancelled


def test_ping_grpc_cancel_during_backoff(grpc_server: GrpcServer, server: MockServer) -> None:
    retry = sdk.RetryPolicy(initial_backoff=5, max_backoff=5, random=lambda: 1.0)
    with client(server, protocol="grpc", grpc_url=grpc_server.url, retry=retry) as c:
        grpc_server.enqueue("traces", GrpcReply(code=grpc.StatusCode.UNAVAILABLE))
        token = sdk.CancelToken()
        threading.Timer(0.2, token.cancel).start()
        started = time.monotonic()
        with pytest.raises(sdk.CancelledError):
            c.ping(cancel=token)
        assert time.monotonic() - started < 3


# --------------------------------------------------------------------------- server version


def test_server_version_from_zpages(server: MockServer) -> None:
    server.enqueue("/debug/servicez", fixture_reply("serverVersion.zpages"))
    with client(server) as c:
        assert c.server_version() == "0.165.0"


def test_server_version_falls_back_to_target_info(server: MockServer) -> None:
    server.enqueue("/debug/servicez", Reply(404, "404 page not found"))
    server.enqueue("/metrics", fixture_reply("serverVersion.telemetry_target_info"))
    with client(server, telemetry_url=server.url + "/metrics") as c:
        assert c.server_version() == "0.165.0"
        server.enqueue("/debug/servicez", Reply(200, "<html>no version</html>"))
        server.enqueue("/metrics", Reply(200, "# nothing here"))
        with pytest.raises(sdk.UnsupportedError):
            c.server_version()
        server.enqueue("/debug/servicez", Reply(404, ""))
        server.enqueue("/metrics", Reply(404, ""))
        with pytest.raises(sdk.UnsupportedError) as info:
            c.server_version()
        assert isinstance(info.value.cause, sdk.NotFoundError)
        server.enqueue("/debug/servicez", Reply(404, ""))
        server.enqueue("/metrics", Reply(401, ""))
        with pytest.raises(sdk.AuthenticationError):
            c.server_version()


def test_server_version_unsupported_and_fatal(server: MockServer) -> None:
    with client(server) as c:
        server.enqueue("/debug/servicez", Reply(404, ""))
        with pytest.raises(sdk.UnsupportedError):
            c.server_version()
        server.enqueue("/debug/servicez", Reply(403, ""))
        with pytest.raises(sdk.PermissionDeniedError):
            c.server_version()


def test_parsers() -> None:
    assert sdk.parse_servicez_version("<b>Version</b></td>\n<td>:</td><td x>v1.2.3</td>") == "1.2.3"
    assert sdk.parse_servicez_version("nothing") is None
    assert sdk.parse_target_info_version('target_info{service_version="v0.1.0"} 1') == "0.1.0"
    assert sdk.parse_target_info_version("up 1") is None


# --------------------------------------------------------------------------- lifecycle


def test_close_owned_resources_and_closed_client(server: MockServer) -> None:
    c = client(server, service_name="svc", metric_export_interval=60)
    handle = c.init_telemetry(set_global=False, traces=False, metrics=False)
    tracing = c.init_tracing(set_global=False)
    logs = c.log_client()
    exporter = c.log_exporter()
    assert isinstance(tracing, sdk.CollectorTracingHandle)
    c.close()
    c.close()
    assert handle.is_shut_down and tracing.is_shut_down
    assert logs.stats().queued == 0
    assert exporter.export([sdk.LogRecord("x")]).error is not None
    for call in (c.health, c.ping, c.server_version, c.wait_until_ready):
        with pytest.raises(sdk.CancelledError):
            call()
    with pytest.raises(sdk.CancelledError):
        c.log_exporter()
    with pytest.raises(sdk.CancelledError):
        c.request(sdk.Request("GET", "/"))


def test_request_and_from_env(server: MockServer) -> None:
    server.enqueue("/raw", Reply(200, {"ok": True}))
    env = {"GROW_OPENTELEMETRY_COLLECTOR_URL": server.url}
    with sdk.OtelCollectorClient.from_env(env) as c:
        assert c.request(sdk.Request("GET", "/raw")).json() == {"ok": True}
        assert c.config.url == server.url


def test_query_clients_are_unsupported(server: MockServer) -> None:
    with client(server) as c:
        tq, lq = c.trace_query(), c.log_query()
        calls = [
            lambda: tq.get_trace("0" * 32),
            tq.search_traces,
            tq.list_services,
            lambda: tq.list_operations("svc"),
            tq.get_dependencies,
            lambda: tq.wait_for_trace("0" * 32),
            lambda: lq.search("x"),
            lambda: lq.tail("x", print),
            lq.list_streams,
            lq.list_labels,
            lambda: lq.wait_for_log("x"),
        ]
        for call in calls:
            with pytest.raises(sdk.UnsupportedError) as info:
                call()
            assert "no query API" in info.value.message


# --------------------------------------------------------------------------- async


async def test_async_client_surface(server: MockServer, grpc_server: GrpcServer) -> None:
    server.enqueue("/", fixture_reply("health.ok"))
    server.enqueue("/v1/traces", fixture_reply("ping.http_empty_export"))
    server.enqueue("/debug/servicez", fixture_reply("serverVersion.zpages"))
    server.enqueue("/raw", Reply(200, {"ok": 1}))
    async with aclient(server, token=TOKEN) as c:
        assert await c.health() == HEALTH_OK
        assert await c.ping() >= 0
        assert await c.server_version() == "0.165.0"
        assert (await c.request(sdk.Request("GET", "/raw"))).json() == {"ok": 1}
        assert isinstance(c.health_client, sdk.AsyncCollectorHealthClient)
        server.enqueue("/", fixture_reply("health.ok"))
        assert (await c.wait_until_ready(5, interval_s=0.01)).healthy
        server.enqueue("/v1/traces", fixture_reply("ping.http_unauthenticated_missing_token"))
        with pytest.raises(sdk.AuthenticationError):
            await c.ping()
    async with aclient(server, protocol="grpc", grpc_url=grpc_server.url) as g:
        assert await g.ping() >= 0
        assert await g.ping() >= 0
        grpc_server.enqueue("traces", GrpcReply(code=grpc.StatusCode.PERMISSION_DENIED))
        with pytest.raises(sdk.PermissionDeniedError):
            await g.ping()
        grpc_server.enqueue("traces", GrpcReply(code=grpc.StatusCode.UNAVAILABLE), GrpcReply())
        await g.ping()


async def test_async_health_fallback_and_version(server: MockServer) -> None:
    cfg = make_config(
        server.url,
        health_url=closed_port_url() + "/",
        zpages_url=server.url,
        telemetry_url=server.url + "/metrics",
    )
    async with sdk.AsyncOtelCollectorClient(cfg) as c:
        server.enqueue("/debug/servicez", Reply(200, "<html></html>"))
        assert await c.health() == sdk.HealthStatus(True, "ok", {"source": "zpages"})
        server.enqueue("/debug/servicez", Reply(500, ""))
        assert (await c.health()).status == "unreachable"
        server.enqueue("/debug/servicez", Reply(404, ""))
        server.enqueue("/metrics", fixture_reply("serverVersion.telemetry_target_info"))
        assert await c.server_version() == "0.165.0"
        server.enqueue("/debug/servicez", Reply(404, ""))
        server.enqueue("/metrics", Reply(404, ""))
        with pytest.raises(sdk.UnsupportedError):
            await c.server_version()
        server.enqueue("/debug/servicez", Reply(200, "nothing"))
        server.enqueue("/metrics", Reply(200, "nothing"))
        with pytest.raises(sdk.UnsupportedError):
            await c.server_version()
        server.enqueue("/debug/servicez", Reply(401, ""))
        with pytest.raises(sdk.AuthenticationError):
            await c.server_version()
        server.enqueue("/debug/servicez", Reply(404, ""))
        server.enqueue("/metrics", Reply(403, ""))
        with pytest.raises(sdk.PermissionDeniedError):
            await c.server_version()
        server.defaults["/debug/servicez"] = Reply(500, "")
        with pytest.raises(sdk.TimeoutError):
            await c.wait_until_ready(0.05, interval_s=0.01)


async def test_async_health_unhealthy_and_cancel(server: MockServer) -> None:
    h = sdk.AsyncCollectorHealthClient.from_env(
        {
            "GROW_OPENTELEMETRY_COLLECTOR_URL": server.url,
            "GROW_OPENTELEMETRY_COLLECTOR_HEALTH_URL": server.url + "/",
        }
    )
    server.enqueue("/", Reply(503, {"status": "Server not available"}))
    assert (await h.health()).status == "unhealthy"
    server.enqueue("/", Reply(200, {}, delay=2))
    task = asyncio.ensure_future(h.health())
    await asyncio.sleep(0.1)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    await h.aclose()
    assert "AsyncCollectorHealthClient(" in repr(h)


async def test_async_owned_resources_and_close(server: MockServer) -> None:
    c = aclient(server, service_name="svc")
    handle = c.init_telemetry(set_global=False, traces=False, metrics=False)
    tracing = c.init_tracing(set_global=False)
    logs = c.log_client()
    exporter = c.log_exporter()
    tq, lq = c.trace_query(), c.log_query()
    for call in (
        lambda: tq.get_trace("x"),
        tq.search_traces,
        tq.list_services,
        lambda: tq.list_operations("s"),
        tq.get_dependencies,
        lambda: tq.wait_for_trace("x"),
        lambda: lq.search("x"),
        lambda: lq.tail("x", print),
        lq.list_streams,
        lq.list_labels,
        lambda: lq.wait_for_log("x"),
    ):
        with pytest.raises(sdk.UnsupportedError):
            await call()
    await c.aclose()
    await c.close()
    assert handle.is_shut_down and tracing.is_shut_down
    assert logs.stats().queued == 0
    assert (await exporter.export([sdk.LogRecord("x")])).error is not None
    for factory in (c.init_telemetry, c.init_tracing, c.log_client, c.log_exporter):
        with pytest.raises(sdk.CancelledError):
            factory()
    with pytest.raises(sdk.CancelledError):
        await c.ping()
    assert "AsyncOtelCollectorClient(" in repr(c)
    assert sdk.AsyncOtelCollectorClient.from_env({}).config.url == sdk.DEFAULT_URL


def test_health_client_from_env_and_repr() -> None:
    h = sdk.CollectorHealthClient.from_env({"GROW_OTEL_URL": "http://alias:4318"})
    assert h.config.url == "http://alias:4318"
    assert "CollectorHealthClient(" in repr(h)
    with h:
        pass
