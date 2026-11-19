"""OTLP log export: protobuf encoding, OtlpLogExporter / AsyncOtlpLogExporter over HTTP and gRPC
(success, partial success, error mapping, shutdown) and the create_log_client helpers."""

from __future__ import annotations

import base64
import gzip
from datetime import datetime, timedelta, timezone
from typing import Any

import grpc
import pytest
from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import (
    ExportLogsServiceRequest,
    ExportLogsServiceResponse,
)

import grow_opentelemetry_collector as sdk
from _grpc import GrpcReply, GrpcServer
from _support import MockServer, Reply, fixture_reply, make_config

TS = datetime(2026, 11, 18, 12, 0, 0, 123456, tzinfo=timezone.utc)
TRACE_ID = "4bf92f3577b34da6a3ce929d0e0e4736"
SPAN_ID = "00f067aa0ba902b7"
PARTIAL = base64.b16decode("0A050802120178")  # partial_success{rejected:2, error_message:"x"}


def record(body: str = "hello", **kw: Any) -> sdk.LogRecord:
    values: dict[str, Any] = {
        "timestamp": TS,
        "severity": sdk.Severity.ERROR,
        "attributes": {"n": 7},
        "resource": {"service.name": "svc"},
    }
    values.update(kw)
    return sdk.LogRecord(body, **values)


def decode(seen_body: bytes, headers: dict[str, str]) -> ExportLogsServiceRequest:
    if headers.get("Content-Encoding") == "gzip":
        seen_body = gzip.decompress(seen_body)
    return ExportLogsServiceRequest.FromString(seen_body)


# --------------------------------------------------------------------------- encoding


def test_encode_log_records() -> None:
    records = [
        record(
            trace_id=TRACE_ID,
            span_id=SPAN_ID,
            trace_flags=1,
            observed_timestamp=TS + timedelta(seconds=1),
            attributes={
                "s": "x",
                "i": 3,
                "big": 2**70,
                "f": 1.5,
                "b": True,
                "bytes": b"\x01",
                "list": [1, "a"],
                "map": {"k": {"n": None}},
            },
        ),
        record("second", resource={"service.name": "other"}, trace_id="zz", span_id="abc"),
        record("third", timestamp=TS.replace(tzinfo=None)),
    ]
    req = sdk.encode_log_records(records)
    assert len(req.resource_logs) == 2
    first = req.resource_logs[0]
    assert first.resource.attributes[0].key == "service.name"
    assert first.scope_logs[0].scope.name == sdk.LOG_SCOPE_NAME
    pb = first.scope_logs[0].log_records[0]
    assert pb.time_unix_nano == int(TS.timestamp()) * 10**9 + 123456000
    assert pb.observed_time_unix_nano == pb.time_unix_nano + 1_000_000_000
    assert (pb.severity_number, pb.severity_text, pb.body.string_value) == (17, "ERROR", "hello")
    assert pb.trace_id.hex() == TRACE_ID and pb.span_id.hex() == SPAN_ID and pb.flags == 1
    attrs = {kv.key: kv.value for kv in pb.attributes}
    assert attrs["i"].int_value == 3 and attrs["big"].string_value == str(2**70)
    assert attrs["f"].double_value == 1.5 and attrs["b"].bool_value
    assert attrs["bytes"].bytes_value == b"\x01"
    assert [v.int_value for v in attrs["list"].array_value.values][:1] == [1]
    assert attrs["map"].kvlist_value.values[0].key == "k"
    third = first.scope_logs[0].log_records[1]
    assert third.time_unix_nano == pb.time_unix_nano
    second = req.resource_logs[1].scope_logs[0].log_records[0]
    assert second.trace_id == b"" and second.span_id == b""


# --------------------------------------------------------------------------- HTTP exporter


def test_http_export_success_and_gzip(server: MockServer) -> None:
    server.enqueue("/v1/logs", fixture_reply("exportLogs.http_protobuf_ok"))
    exporter = sdk.OtlpLogExporter(make_config(server.url, token="tok-9876"))
    assert exporter.endpoint == server.url + "/v1/logs"
    result = exporter.export([record()], gzip=True, timeout_s=5)
    assert result == sdk.ExportResult(accepted=1, rejected=0, error=None, retries=0)
    seen = server.seen[0]
    assert seen.headers["Content-Type"] == "application/x-protobuf"
    assert seen.headers["Content-Encoding"] == "gzip"
    assert seen.headers["Authorization"] == "Bearer tok-9876"
    req = decode(seen.body, seen.headers)
    assert req.resource_logs[0].scope_logs[0].log_records[0].body.string_value == "hello"
    assert exporter.export([]) == sdk.ExportResult(accepted=0)
    server.enqueue("/v1/logs", Reply(200, b""))
    exporter.export([record()], gzip=False)
    assert "Content-Encoding" not in server.seen[1].headers
    exporter.shutdown()
    exporter.shutdown()
    closed = exporter.export([record()])
    assert closed.rejected == 1 and isinstance(closed.error, sdk.ValidationError)


def test_http_export_compression_none_and_signal_url(server: MockServer) -> None:
    server.enqueue("/custom", Reply(200, b"garbage-not-proto\xff"))
    cfg = make_config(
        server.url,
        compression="none",
        logs_url=server.url + "/custom",
        logs_headers={"X-Logs": "1"},
    )
    exporter = sdk.OtlpLogExporter(cfg)
    assert exporter.export([record()], gzip=True).accepted == 1  # malformed body = accepted
    assert "Content-Encoding" not in server.seen[0].headers
    assert server.seen[0].headers["X-Logs"] == "1"
    exporter.shutdown()


def test_http_export_partial_success(server: MockServer) -> None:
    server.enqueue("/v1/logs", Reply(200, PARTIAL, {"Content-Type": "application/x-protobuf"}))
    exporter = sdk.OtlpLogExporter(make_config(server.url))
    result = exporter.export([record(), record("b"), record("c")])
    assert (result.accepted, result.rejected) == (1, 2)
    assert isinstance(result.error, sdk.ValidationError)
    assert "rejected 2 of 3 records (x)" in result.error.message
    assert not result.error.retryable
    resp = ExportLogsServiceResponse()
    resp.partial_success.rejected_log_records = 5
    server.enqueue("/v1/logs", Reply(200, resp.SerializeToString()))
    result = exporter.export([record()])
    assert (result.accepted, result.rejected) == (0, 1)
    assert result.error is not None and "()" not in result.error.message


@pytest.mark.parametrize(
    ("name", "cls"),
    [
        ("export.http_bad_request_protobuf", sdk.ValidationError),
        ("export.http_bad_request_json", sdk.ValidationError),
        ("export.http_payload_too_large", sdk.ValidationError),
        ("export.http_unsupported_media_type", sdk.ValidationError),
        ("export.http_not_found", sdk.NotFoundError),
        ("ping.http_unauthenticated_missing_token", sdk.AuthenticationError),
    ],
)
def test_http_export_errors(server: MockServer, name: str, cls: type[Exception]) -> None:
    server.enqueue("/v1/logs", fixture_reply(name))
    result = sdk.OtlpLogExporter(make_config(server.url)).export([record()])
    assert (result.accepted, result.rejected) == (0, 1)
    assert isinstance(result.error, cls)


def test_http_export_retries_and_network_error(server: MockServer) -> None:
    server.enqueue("/v1/logs", Reply(429, b""), Reply(200, b"\n\x00"))
    exporter = sdk.OtlpLogExporter(make_config(server.url))
    result = exporter.export([record()])
    assert (result.accepted, result.retries) == (1, 1)
    server.enqueue("/v1/logs", Reply(503, b"", {"Retry-After": "1"}))
    unavailable = exporter.export([record()])
    assert isinstance(unavailable.error, sdk.UnavailableError) and unavailable.error.retryable
    refused = sdk.OtlpLogExporter(make_config("http://127.0.0.1:1"))
    result = refused.export([record()])
    assert isinstance(result.error, sdk.UnavailableError)


# --------------------------------------------------------------------------- gRPC exporter


def test_grpc_export(grpc_server: GrpcServer) -> None:
    cfg = make_config(
        "http://127.0.0.1:1", protocol="grpc", grpc_url=grpc_server.url, token="grpc-tok-1"
    )
    exporter = sdk.OtlpLogExporter(cfg)
    assert exporter.endpoint == grpc_server.url
    assert exporter.export([record()], gzip=True).accepted == 1
    seen = grpc_server.seen[0]
    assert seen.metadata["authorization"] == "Bearer grpc-tok-1"
    assert ExportLogsServiceRequest.FromString(seen.body).resource_logs
    grpc_server.enqueue("logs", GrpcReply(body=PARTIAL))
    assert exporter.export([record(), record()]).rejected == 2
    grpc_server.enqueue("logs", GrpcReply(code=grpc.StatusCode.INTERNAL, details="bad"))
    error = exporter.export([record()]).error
    assert isinstance(error, sdk.ValidationError) and "INTERNAL" in error.message
    exporter.shutdown()


# --------------------------------------------------------------------------- async exporter


async def test_async_http_export(server: MockServer) -> None:
    server.enqueue("/v1/logs", fixture_reply("exportLogs.http_protobuf_ok"))
    server.enqueue("/v1/logs", fixture_reply("ping.http_unauthenticated_wrong_token"))
    exporter = sdk.AsyncOtlpLogExporter(make_config(server.url))
    assert (await exporter.export([record()], gzip=True, timeout_s=5)).accepted == 1
    assert decode(server.seen[0].body, server.seen[0].headers).resource_logs
    denied = await exporter.export([record()])
    assert isinstance(denied.error, sdk.AuthenticationError)
    assert await exporter.export([]) == sdk.ExportResult(accepted=0)
    await exporter.shutdown()
    await exporter.aclose()
    assert (await exporter.export([record()])).rejected == 1


async def test_async_grpc_export(grpc_server: GrpcServer) -> None:
    cfg = make_config("http://127.0.0.1:1", protocol="grpc", grpc_url=grpc_server.url)
    exporter = sdk.AsyncOtlpLogExporter(cfg)
    assert (await exporter.export([record()])).accepted == 1
    grpc_server.enqueue("logs", GrpcReply(code=grpc.StatusCode.UNAUTHENTICATED, details="no"))
    assert isinstance((await exporter.export([record()])).error, sdk.AuthenticationError)
    await exporter.shutdown()


async def test_async_export_refused() -> None:
    exporter = sdk.AsyncOtlpLogExporter(make_config("http://127.0.0.1:1"))
    assert isinstance((await exporter.export([record()])).error, sdk.UnavailableError)
    await exporter.shutdown()


# --------------------------------------------------------------------------- log clients


def test_create_log_client_defaults(server: MockServer) -> None:
    cfg = make_config(
        server.url,
        service_name="svc",
        service_version="1.2",
        environment="dev",
        resource_attributes={"team": "core"},
        logs_batch={"flush_interval_s": 0.05, "max_batch_size": 10},
    )
    client = sdk.create_log_client(cfg)
    assert client.processor.options.max_batch_size == 10
    assert client.resource == {
        "team": "core",
        "service.name": "svc",
        "service.version": "1.2",
        "deployment.environment.name": "dev",
    }
    client.logger("app").error("boom", {"password": "hunter2"})
    client.flush(5)
    client.close()
    assert client.stats().sent == 1
    req = decode(server.seen[0].body, server.seen[0].headers)
    pb = req.resource_logs[0].scope_logs[0].log_records[0]
    attrs = {kv.key: kv.value.string_value for kv in pb.attributes}
    assert attrs["password"] == "[REDACTED]" and attrs["logger.name"] == "app"


def test_create_log_client_options(server: MockServer) -> None:
    client = sdk.create_log_client(
        make_config(server.url),
        options=sdk.BatchOptions(max_batch_size=3, max_queue_size=3),
        resource={"service.name": "x"},
        redaction=None,
        correlate=False,
    )
    assert client.redaction is None and client.processor.options.max_batch_size == 3
    client.send(sdk.LogInput("pw", attributes={"password": "visible"}))
    client.close()
    pb = decode(server.seen[0].body, server.seen[0].headers).resource_logs[0]
    assert pb.scope_logs[0].log_records[0].attributes[0].value.string_value == "visible"


async def test_create_async_log_client(server: MockServer) -> None:
    client = sdk.create_async_log_client(make_config(server.url, service_name="asvc"))
    client.send(sdk.LogInput("async hello"))
    await client.flush(5)
    await client.close()
    assert client.stats().sent == 1
    other = sdk.create_async_log_client(
        make_config(server.url),
        resource={"a": "b"},
        redaction=None,
        exporter=sdk.AsyncOtlpLogExporter(make_config(server.url)),
    )
    assert other.resource == {"a": "b"} and other.redaction is None
    await other.close()
