"""OTLP helpers: google.rpc.Status decoding, error text, HTTP and gRPC error mapping, gRPC
targets and credentials."""

from __future__ import annotations

import asyncio
import base64
import json
from typing import Any

import grpc
import httpx
import pytest

import grow_opentelemetry_collector as sdk
from _grpc import GrpcReply, GrpcServer
from _support import fixture

UNAUTH = base64.b64decode(
    fixture("ping.http_unauthenticated_wrong_token")["response"]["body"]["base64"]
)


def test_decode_rpc_status() -> None:
    assert sdk.decode_rpc_status(UNAUTH) == (
        16,
        "provided authorization does not match expected scheme or token",
    )
    assert sdk.decode_rpc_status(b"") == (None, None)
    assert sdk.decode_rpc_status(b"\x80") is None  # truncated varint
    assert sdk.decode_rpc_status(b"\x08\x80") is None  # truncated value
    assert sdk.decode_rpc_status(b"\x12\x05ab") is None  # length beyond the end
    assert sdk.decode_rpc_status(b"\x0d\x00\x00\x00\x00") is None  # fixed32 wire type
    assert sdk.decode_rpc_status(b"\x1a\x01x\x08\x03") == (3, None)  # unknown field skipped


def test_otlp_error_text() -> None:
    assert sdk.otlp_error_text(None) is None
    assert sdk.otlp_error_text(b"") is None
    assert sdk.otlp_error_text(UNAUTH).startswith("provided authorization")
    assert sdk.otlp_error_text('{"code":3,"message":"ReadObject: bad"}') == "ReadObject: bad"
    assert sdk.otlp_error_text("[1, 2]") == "[1, 2]"
    assert sdk.otlp_error_text("404 page not found\n") == "404 page not found"
    assert sdk.otlp_error_text(b"\xff\xfe") is None  # binary garbage only
    assert sdk.otlp_error_text("   ") is None
    assert (
        sdk.otlp_error_text("token secret-123 leaked", ["secret-123"]) == "token [REDACTED] leaked"
    )
    assert len(sdk.otlp_error_text("x" * 1000) or "") == 300


def response(status: int, body: bytes, **headers: str) -> Any:
    from grow_opentelemetry_collector._core.http import Response

    return Response(status, httpx.Headers(headers), body)


def test_error_for_http_response() -> None:
    err = sdk.error_for_http_response("ping", response(401, UNAUTH))
    assert isinstance(err, sdk.AuthenticationError)
    assert err.message.startswith("ping: provided authorization") and err.http_status == 401
    limited = sdk.error_for_http_response("x", response(429, b"", **{"Retry-After": "3"}))
    assert isinstance(limited, sdk.RateLimitError) and limited.retry_after == 3.0
    plain = sdk.error_for_http_response("x", response(502, b""))
    assert plain.message == "x: bad gateway (HTTP 502)" and plain.retryable


@pytest.mark.parametrize(
    ("status", "cls"),
    [
        (1, sdk.CancelledError),
        (3, sdk.ValidationError),
        (4, sdk.TimeoutError),
        (5, sdk.NotFoundError),
        (6, sdk.ConflictError),
        (7, sdk.PermissionDeniedError),
        (8, sdk.RateLimitError),
        (12, sdk.NotFoundError),
        (13, sdk.ValidationError),
        (14, sdk.UnavailableError),
        (16, sdk.AuthenticationError),
        (2, sdk.UnavailableError),
        (10, sdk.UnavailableError),
        (15, sdk.UnavailableError),
    ],
)
def test_error_for_grpc_status(status: int, cls: type[Exception]) -> None:
    err = sdk.error_for_grpc_status("op", status, None)
    assert isinstance(err, cls)
    assert err.message.startswith("op: ") and "(gRPC " in err.message
    assert sdk.code_for_grpc_status(status) == err.code


def test_error_for_grpc_status_details() -> None:
    err = sdk.error_for_grpc_status("op", 99, "weird k-1234", ["k-1234"], retryable=False)
    assert err.message == "op: weird [REDACTED] (gRPC 99)" and not err.retryable


def test_grpc_target_and_credentials(tmp_path: Any) -> None:
    assert sdk.grpc_target("http://collector:4317") == "collector:4317"
    assert sdk.grpc_target("https://collector") == "collector:443"
    assert sdk.grpc_target("http://collector") == "collector:80"
    assert sdk.grpc_target("http://[::1]:4317") == "[::1]:4317"
    with pytest.raises(sdk.ConfigError):
        sdk.grpc_target("collector:4317")
    assert sdk.grpc_credentials("http://c:4317", None) is None
    assert sdk.grpc_credentials("https://c:4317", None) is not None
    cert, key = tmp_path / "c.pem", tmp_path / "k.pem"
    cert.write_text("cert")
    key.write_text("key")
    tls = sdk.TlsConfig(client_cert_path=str(cert), client_key_path=str(key))
    assert sdk.grpc_credentials("http://c:4317", tls) is not None
    with pytest.raises(sdk.ConfigError):
        sdk.grpc_credentials("http://c:4317", sdk.TlsConfig(insecure_skip_verify=True))
    secure = sdk.open_channel("https://c:4317", None)
    secure.close()
    path = sdk.pem_file("PEM")
    with open(path, encoding="utf-8") as fh:
        assert fh.read() == "PEM"
    assert sdk.read_tls_file(None) is None
    assert sdk.split_url("http://h:1/a?b=1") == ("http://h:1", "/a?b=1")
    assert sdk.split_url("http://h:1") == ("http://h:1", "/")


async def test_aio_channel_secure_and_grpc_call_async_retry(grpc_server: GrpcServer) -> None:
    secure = sdk.open_aio_channel("https://c:4317", None)
    await secure.close()
    channel = sdk.open_aio_channel(grpc_server.url, None)
    retry = sdk.RetryPolicy(max_retries=1, initial_backoff=0.01, random=lambda: 0.0)
    grpc_server.enqueue("logs", GrpcReply(code=grpc.StatusCode.UNAVAILABLE), GrpcReply(body=b"ok"))
    got = await sdk.grpc_call_async(
        channel, "op", sdk.GRPC_EXPORT_PATHS["logs"], b"", metadata=[], timeout=5, retry=retry
    )
    assert got == b"ok"
    grpc_server.enqueue(
        "logs",
        GrpcReply(code=grpc.StatusCode.UNAVAILABLE),
        GrpcReply(code=grpc.StatusCode.UNAVAILABLE),
    )
    with pytest.raises(sdk.UnavailableError):
        await sdk.grpc_call_async(
            channel, "op", sdk.GRPC_EXPORT_PATHS["logs"], b"", metadata=[], timeout=5, retry=retry
        )
    grpc_server.enqueue("logs", GrpcReply(delay=1.0))
    with pytest.raises(sdk.TimeoutError):
        await sdk.grpc_call_async(
            channel,
            "op",
            sdk.GRPC_EXPORT_PATHS["logs"],
            b"",
            metadata=[],
            timeout=0.1,
            retry=retry,
        )
    grpc_server.enqueue("logs", GrpcReply(delay=2.0))
    sink: list[BaseException] = []

    async def run() -> None:
        try:
            await sdk.grpc_call_async(
                channel,
                "op",
                sdk.GRPC_EXPORT_PATHS["logs"],
                b"",
                metadata=[],
                timeout=5,
                retry=retry,
            )
        except BaseException as exc:
            sink.append(exc)
            raise

    task = asyncio.ensure_future(run())
    await asyncio.sleep(0.1)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert isinstance(sink[0], sdk.CancelledError)
    slow = sdk.RetryPolicy(max_retries=3, initial_backoff=5, max_backoff=5, random=lambda: 1.0)
    grpc_server.enqueue("logs", GrpcReply(code=grpc.StatusCode.UNAVAILABLE))
    sink.clear()

    async def backoff() -> None:
        try:
            await sdk.grpc_call_async(
                channel,
                "op",
                sdk.GRPC_EXPORT_PATHS["logs"],
                b"",
                metadata=[],
                timeout=5,
                retry=slow,
            )
        except BaseException as exc:
            sink.append(exc)
            raise

    task = asyncio.ensure_future(backoff())
    await asyncio.sleep(0.3)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert isinstance(sink[0], sdk.CancelledError)
    await channel.close()


def test_grpc_call_cancelled_rpc(grpc_server: GrpcServer) -> None:
    grpc_server.enqueue("traces", GrpcReply(code=grpc.StatusCode.CANCELLED))
    channel = sdk.open_channel(grpc_server.url, None)
    with pytest.raises(sdk.CancelledError):
        sdk.grpc_call(
            channel,
            "op",
            sdk.GRPC_EXPORT_PATHS["traces"],
            b"",
            metadata=[],
            timeout=5,
            retry=sdk.RetryPolicy(max_retries=0),
        )
    channel.close()
    assert json.loads(json.dumps(dict(sdk.GRPC_EXPORT_PATHS)))["metrics"].endswith("/Export")
    assert sdk.USER_AGENT.endswith("(python)")
