"""Shared OTLP plumbing: error mapping of OTLP responses (HTTP status + ``google.rpc.Status``
body, gRPC status codes) to the SDK error classes, gRPC channels and unary calls with the
SDK retry policy, and TLS helpers for the official OpenTelemetry exporters.

Service-owned module.
"""

from __future__ import annotations

import asyncio
import atexit
import contextlib
import json
import os
import re
import tempfile
import threading
from collections.abc import Callable, Mapping
from typing import TYPE_CHECKING, Any
from urllib.parse import urlsplit, urlunsplit

from ._core import (
    CancelledError,
    CancelToken,
    ConfigError,
    ErrorCode,
    RetryPolicy,
    ServiceError,
    TimeoutError,
    TlsConfig,
    error_for_code,
    error_from_status,
    parse_retry_after,
    redact_text,
    truncate,
    user_agent,
)
from ._core.http import Response

if TYPE_CHECKING:
    import grpc
    import grpc.aio

__all__ = [
    "GRPC_EXPORT_PATHS",
    "USER_AGENT",
    "code_for_grpc_status",
    "decode_rpc_status",
    "error_for_grpc_status",
    "error_for_http_response",
    "grpc_call",
    "grpc_call_async",
    "grpc_credentials",
    "grpc_metadata",
    "grpc_target",
    "open_aio_channel",
    "open_channel",
    "otlp_error_text",
    "pem_file",
    "read_tls_file",
    "split_url",
]

#: ``grow-codes-opentelemetry-collector/<version> (python)``.
USER_AGENT = user_agent()

#: gRPC method paths of the three OTLP export services.
GRPC_EXPORT_PATHS: Mapping[str, str] = {
    "traces": "/opentelemetry.proto.collector.trace.v1.TraceService/Export",
    "logs": "/opentelemetry.proto.collector.logs.v1.LogsService/Export",
    "metrics": "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export",
}

_GRPC_NAMES = (
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
)
_CONTROL = re.compile(r"[^\x20-\x7e]+")


# --------------------------------------------------------------------------- error bodies


def _varint(data: bytes, pos: int) -> tuple[int, int] | None:
    value = 0
    shift = 0
    while pos < len(data) and shift < 64:
        byte = data[pos]
        value |= (byte & 0x7F) << shift
        pos += 1
        if not byte & 0x80:
            return value, pos
        shift += 7
    return None


def decode_rpc_status(data: bytes) -> tuple[int | None, str | None] | None:
    """Decode a binary ``google.rpc.Status`` (field 1 ``code``, field 2 ``message``).

    Returns ``(code, message)`` or ``None`` when the bytes are not a well-formed Status.
    """
    code: int | None = None
    message: str | None = None
    pos = 0
    while pos < len(data):
        tag = _varint(data, pos)
        if tag is None:
            return None
        key, pos = tag
        field, wire = key >> 3, key & 7
        if wire == 0:
            value = _varint(data, pos)
            if value is None:
                return None
            if field == 1:
                code = value[0]
            pos = value[1]
        elif wire == 2:
            length = _varint(data, pos)
            if length is None or length[1] + length[0] > len(data):
                return None
            start, end = length[1], length[1] + length[0]
            if field == 2:
                message = data[start:end].decode("utf-8", errors="replace")
            pos = end
        else:
            return None
    return code, message


def otlp_error_text(body: bytes | str | None, known_secrets: list[str] | None = None) -> str | None:
    """Human text of an OTLP error body: JSON ``{"code","message"}``, a binary
    ``google.rpc.Status`` or plain text. Redacted, truncated to 300 characters; ``None`` for
    an empty body."""
    if not body:
        return None
    raw = body.encode("utf-8") if isinstance(body, str) else body
    text = raw.decode("utf-8", errors="replace")
    message: str | None = None
    try:
        doc = json.loads(text)
    except ValueError:
        status = decode_rpc_status(raw)
        message = status[1] if status is not None and status[1] is not None else None
        if message is None:
            message = _CONTROL.sub(" ", text)
    else:
        if isinstance(doc, Mapping) and isinstance(doc.get("message"), str):
            message = doc["message"]
        else:
            message = text
    clean = " ".join(message.split())
    if not clean:
        return None
    return truncate(redact_text(clean, known_secrets or []), 300)


def error_for_http_response(
    operation: str, resp: Response, known_secrets: list[str] | None = None
) -> ServiceError:
    """The SDK error for an OTLP/HTTP error response (status per SPEC 2.1, message from the
    JSON or protobuf ``google.rpc.Status`` body, ``retry_after`` on 429/503)."""
    text = otlp_error_text(resp.content, known_secrets)
    content = json.dumps({"message": text}).encode() if text else b""
    headers = resp.headers.copy()
    headers["content-type"] = "application/json"
    retry_after = None
    if resp.status in (429, 503):
        retry_after = parse_retry_after(resp.headers.get("retry-after"))
    return error_from_status(
        resp.status,
        operation,
        content=content,
        headers=headers,
        known_secrets=known_secrets,
        retry_after=retry_after,
    )


def code_for_grpc_status(status: int) -> ErrorCode:
    """Map a gRPC status code to an SDK :class:`ErrorCode`."""
    if status == 1:
        return ErrorCode.CANCELLED
    if status in (3, 9, 11, 13):
        return ErrorCode.VALIDATION
    if status == 4:
        return ErrorCode.TIMEOUT
    if status in (5, 12):
        return ErrorCode.NOT_FOUND
    if status in (6, 10):
        return ErrorCode.CONFLICT
    if status == 7:
        return ErrorCode.PERMISSION_DENIED
    if status == 8:
        return ErrorCode.RATE_LIMITED
    if status == 16:
        return ErrorCode.AUTHENTICATION
    return ErrorCode.UNAVAILABLE


def error_for_grpc_status(
    operation: str,
    status: int,
    message: str | None,
    known_secrets: list[str] | None = None,
    retryable: bool | None = None,
) -> ServiceError:
    """The SDK error for a gRPC status: ``"<op>: <message> (gRPC <NAME>)"``."""
    code = code_for_grpc_status(status)
    text = otlp_error_text(message, known_secrets) or code.value.lower().replace("_", " ")
    name = _GRPC_NAMES[status] if 0 <= status < len(_GRPC_NAMES) else str(status)
    return error_for_code(code)(f"{operation}: {text} (gRPC {name})", retryable=retryable)


def split_url(url: str) -> tuple[str, str]:
    """``(scheme://host:port, /path?query)`` of an absolute URL (path defaults to ``/``)."""
    parts = urlsplit(url)
    base = urlunsplit((parts.scheme, parts.netloc, "", "", ""))
    path = parts.path or "/"
    if parts.query:
        path += "?" + parts.query
    return base, path


# --------------------------------------------------------------------------- TLS helpers

_PEM_FILES: list[str] = []
_PEM_LOCK = threading.Lock()


def _cleanup_pem_files() -> None:
    for path in _PEM_FILES:
        with contextlib.suppress(OSError):
            os.unlink(path)


atexit.register(_cleanup_pem_files)


def pem_file(pem: str) -> str:
    """Write an inline PEM bundle to a private temp file (removed at exit); return its path."""
    fd, path = tempfile.mkstemp(prefix="grow-otelcol-ca-", suffix=".pem")
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(pem)
    with _PEM_LOCK:
        _PEM_FILES.append(path)
    return path


def read_tls_file(path: str | None) -> bytes | None:
    """Bytes of a TLS file; :class:`ConfigError` (without the path) when unreadable."""
    if path is None:
        return None
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except OSError:
        raise ConfigError("otlp: cannot read a TLS file (check the configured path)") from None


# --------------------------------------------------------------------------- gRPC


def grpc_module() -> Any:
    """The ``grpc`` module; :class:`ConfigError` when the ``grpc`` extra is missing."""
    try:
        import grpc  # noqa: PLC0415 - optional extra
    except ImportError as exc:  # pragma: no cover - depends on the installed extras
        raise ConfigError(
            "otlp: protocol grpc needs the 'grpc' extra "
            "(pip install 'grow-opentelemetry-collector[grpc]')"
        ) from exc
    return grpc


def grpc_target(url: str) -> str:
    """``host:port`` of a gRPC URL (``http://collector:4317`` → ``collector:4317``)."""
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        raise ConfigError("otlp: the gRPC URL must be an http or https URL")
    host = parts.hostname
    if ":" in host:
        host = f"[{host}]"
    port = parts.port or (443 if parts.scheme == "https" else 80)
    return f"{host}:{port}"


def grpc_credentials(url: str, tls: TlsConfig | None) -> grpc.ChannelCredentials | None:
    """TLS channel credentials for an ``https://`` target or when TLS material is set;
    ``None`` for plaintext. ``insecure_skip_verify`` raises :class:`ConfigError` (grpcio
    cannot skip certificate verification)."""
    grpc_mod = grpc_module()
    tls = tls or TlsConfig()
    if tls.insecure_skip_verify:
        raise ConfigError("otlp: insecure_skip_verify is not supported over gRPC")
    ca = read_tls_file(tls.ca_cert_path)
    if tls.ca_cert_pem:
        ca = tls.ca_cert_pem.encode() + (b"\n" + ca if ca else b"")
    cert = read_tls_file(tls.client_cert_path)
    key = read_tls_file(tls.client_key_path)
    if not (url.startswith("https:") or ca is not None or cert is not None):
        return None
    creds: grpc.ChannelCredentials = grpc_mod.ssl_channel_credentials(
        root_certificates=ca, private_key=key, certificate_chain=cert
    )
    return creds


def _channel_options() -> list[tuple[str, Any]]:
    return [("grpc.primary_user_agent", USER_AGENT)]


def open_channel(url: str, tls: TlsConfig | None) -> grpc.Channel:
    """A sync gRPC channel for ``url``."""
    grpc_mod = grpc_module()
    target = grpc_target(url)
    creds = grpc_credentials(url, tls)
    if creds is None:
        channel: grpc.Channel = grpc_mod.insecure_channel(target, options=_channel_options())
    else:
        channel = grpc_mod.secure_channel(target, creds, options=_channel_options())
    return channel


def open_aio_channel(url: str, tls: TlsConfig | None) -> grpc.aio.Channel:
    """An asyncio gRPC channel for ``url``."""
    grpc_mod = grpc_module()
    target = grpc_target(url)
    creds = grpc_credentials(url, tls)
    if creds is None:
        channel: grpc.aio.Channel = grpc_mod.aio.insecure_channel(
            target, options=_channel_options()
        )
    else:
        channel = grpc_mod.aio.secure_channel(target, creds, options=_channel_options())
    return channel


def grpc_metadata(headers: Mapping[str, str]) -> list[tuple[str, str]]:
    """gRPC metadata from export headers (names lower-cased)."""
    return [(k.lower(), v) for k, v in headers.items()]


def _compression(gzip: bool) -> Any:
    grpc_mod = grpc_module()
    return grpc_mod.Compression.Gzip if gzip else grpc_mod.Compression.NoCompression


def _map_rpc_error(
    operation: str, err: Any, timeout: float, secrets: list[str]
) -> tuple[ServiceError, bool]:
    """``(error, may_retry)`` for a ``grpc.RpcError``."""
    code = err.code()
    number = code.value[0] if code is not None else 2
    if number == 1:
        return CancelledError(f"{operation}: cancelled"), False
    if number == 4:
        return TimeoutError(f"{operation}: request timed out after {timeout:g}s"), False
    error = error_for_grpc_status(operation, number, err.details(), secrets)
    return error, error.retryable


def grpc_call(
    channel: grpc.Channel,
    operation: str,
    path: str,
    payload: bytes,
    *,
    metadata: list[tuple[str, str]],
    timeout: float,
    retry: RetryPolicy,
    gzip: bool = False,
    known_secrets: Callable[[], list[str]] = list,
    cancel: CancelToken | None = None,
) -> bytes:
    """One unary OTLP call with raw protobuf bytes; retries retryable statuses
    (UNAVAILABLE, RESOURCE_EXHAUSTED …) with ``retry`` backoff. Returns the response bytes."""
    method = channel.unary_unary(path)
    compression = _compression(gzip)
    attempt = 0
    while True:
        attempt += 1
        if cancel is not None and cancel.cancelled:
            raise CancelledError(f"{operation}: cancelled")
        future = method.future(payload, timeout=timeout, metadata=metadata, compression=compression)
        unregister = cancel.on_cancel(future.cancel) if cancel is not None else None
        try:
            result: bytes = future.result()
            return result
        except Exception as err:  # noqa: BLE001 - grpc.RpcError / FutureCancelledError
            if cancel is not None and cancel.cancelled:
                raise CancelledError(f"{operation}: cancelled") from None
            if not hasattr(err, "code"):
                raise CancelledError(f"{operation}: cancelled") from None
            error, may_retry = _map_rpc_error(operation, err, timeout, known_secrets())
            if not may_retry or attempt > retry.max_retries:
                raise error from None
        finally:
            if unregister is not None:
                unregister()
        wake = threading.Event()
        undo = cancel.on_cancel(wake.set) if cancel is not None else None
        wake.wait(retry.backoff(attempt - 1))
        if undo is not None:
            undo()


async def grpc_call_async(
    channel: grpc.aio.Channel,
    operation: str,
    path: str,
    payload: bytes,
    *,
    metadata: list[tuple[str, str]],
    timeout: float,
    retry: RetryPolicy,
    gzip: bool = False,
    known_secrets: Callable[[], list[str]] = list,
) -> bytes:
    """Async :func:`grpc_call`; cancel by cancelling the task (raises :class:`CancelledError`)."""
    method = channel.unary_unary(path)
    compression = _compression(gzip)
    attempt = 0
    while True:
        attempt += 1
        try:
            result: bytes = await method(
                payload, timeout=timeout, metadata=metadata, compression=compression
            )
            return result
        except asyncio.CancelledError as exc:
            if isinstance(exc, CancelledError):
                raise
            raise CancelledError(f"{operation}: cancelled", cause=exc) from exc
        except Exception as err:
            if not hasattr(err, "code"):
                raise
            error, may_retry = _map_rpc_error(operation, err, timeout, known_secrets())
            if not may_retry or attempt > retry.max_retries:
                raise error from None
        try:
            await asyncio.sleep(retry.backoff(attempt - 1))
        except asyncio.CancelledError as exc:
            raise CancelledError(f"{operation}: cancelled", cause=exc) from exc
