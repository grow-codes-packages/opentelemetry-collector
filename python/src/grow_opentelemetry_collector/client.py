"""OpenTelemetry Collector clients (sync and async): health (``health_check`` / zpages), an
authenticated OTLP ping, the collector version, and factories for the OTLP exporters whose
lifetime the client owns (``close()`` flushes and shuts them down).

This file is owned by the service package (generated once by sdk-kit).
"""

from __future__ import annotations

import asyncio
import contextlib
import inspect
import threading
import time
from collections.abc import Callable, Mapping
from datetime import timedelta
from types import TracebackType
from typing import TYPE_CHECKING, Any, TypeVar

import httpx

from ._core import (
    AsyncHttpTransport,
    CancelledError,
    CancelToken,
    ConfigError,
    HealthStatus,
    HttpTransport,
    Request,
    Response,
)
from ._logging import AsyncBatchingLogClient, BatchingLogClient
from ._tracing import TracingConfig
from .config import OtelCollectorConfig
from .health import (
    DEFAULT_READY_INTERVAL_S,
    DEFAULT_READY_TIMEOUT_S,
    AsyncCollectorHealthClient,
    CollectorHealthClient,
    transport_config,
)
from .logs import AsyncOtlpLogExporter, OtlpLogExporter, create_async_log_client, create_log_client
from .otlp import (
    GRPC_EXPORT_PATHS,
    error_for_http_response,
    grpc_call,
    grpc_call_async,
    grpc_metadata,
    open_aio_channel,
    open_channel,
    split_url,
)
from .query import (
    AsyncCollectorLogQueryClient,
    AsyncCollectorTraceQueryClient,
    CollectorLogQueryClient,
    CollectorTraceQueryClient,
)
from .setup import CollectorTracingHandle, init_tracing
from .telemetry import TelemetryHandle, init_telemetry

if TYPE_CHECKING:
    import grpc
    import grpc.aio

__all__ = ["AsyncOtelCollectorClient", "OtelCollectorClient"]

_T = TypeVar("_T")


def _ping_request(cfg: OtelCollectorConfig, path: str) -> Request:
    return Request(
        "POST",
        path,
        operation="ping",
        content=b"",
        headers={
            "Content-Type": "application/x-protobuf",
            "Accept": "application/x-protobuf",
            **dict(cfg.traces_headers),
        },
        idempotent=True,
        raise_for_status=False,
    )


def _closed_error(operation: str) -> ConfigError:
    return ConfigError(f"{operation}: client is closed")


class _ClientBase:
    def __init__(self, config: OtelCollectorConfig | None) -> None:
        cfg = (config if config is not None else OtelCollectorConfig.from_env()).validate()
        self.config = cfg
        base, self._ping_path = split_url(cfg.signal_url("traces"))
        self._transport_cfg = transport_config(cfg, base, cfg.headers)
        self._lock = threading.Lock()
        self._closed = False

    def _check_open(self, operation: str) -> None:
        if self._closed:
            raise CancelledError(f"{operation}: client is closed")

    def _ping_response(self, resp: Response) -> None:
        if not resp.ok:
            raise error_for_http_response("ping", resp, self.config.known_secrets())

    def __repr__(self) -> str:
        return f"{type(self).__name__}(config={self.config!r})"


class OtelCollectorClient(_ClientBase):
    """Synchronous OpenTelemetry Collector client. Thread-safe; use as a context manager or
    call :meth:`close`. Every I/O method accepts ``cancel=`` (a :class:`CancelToken`)."""

    def __init__(
        self,
        config: OtelCollectorConfig | None = None,
        *,
        http_transport: httpx.BaseTransport | None = None,
    ) -> None:
        super().__init__(config)
        self._transport = HttpTransport(
            self._transport_cfg,
            known_secrets=self.config.known_secrets,
            http_transport=http_transport,
        )
        self._health = CollectorHealthClient(self.config, http_transport=http_transport)
        self._channel: grpc.Channel | None = None
        self._owned: list[Any] = []

    @classmethod
    def from_env(
        cls,
        env: Mapping[str, str] | None = None,
        **overrides: Any,
    ) -> OtelCollectorClient:
        """Build a client from ``GROW_OPENTELEMETRY_COLLECTOR_*``, ``GROW_OTEL_*`` and the
        standard ``OTEL_*`` variables (see :meth:`OtelCollectorConfig.from_env`)."""
        return cls(OtelCollectorConfig.from_env(env, **overrides))

    @property
    def health_client(self) -> CollectorHealthClient:
        """The health client (``health_check`` extension with zpages fallback)."""
        return self._health

    def request(self, req: Request, *, cancel: CancelToken | None = None) -> Response:
        """Send a raw request to the OTLP/HTTP receiver (retries, hooks, redaction)."""
        return self._transport.request(req, cancel=cancel)

    def health(self, *, cancel: CancelToken | None = None) -> HealthStatus:
        """Unauthenticated health: ``GET {health_url}`` (port 13133), falling back to zpages
        when that endpoint is unreachable. Never raises for an unhealthy or unreachable
        collector (see :meth:`CollectorHealthClient.health`)."""
        self._check_open("health")
        return self._health.health(cancel=cancel)

    def wait_until_ready(
        self,
        timeout_s: float | timedelta = DEFAULT_READY_TIMEOUT_S,
        *,
        interval_s: float | timedelta = DEFAULT_READY_INTERVAL_S,
        cancel: CancelToken | None = None,
    ) -> HealthStatus:
        """Poll :meth:`health` until healthy; :class:`TimeoutError` after ``timeout_s``."""
        self._check_open("wait_until_ready")
        return self._health.wait_until_ready(timeout_s, interval_s=interval_s, cancel=cancel)

    def ping(self, *, cancel: CancelToken | None = None) -> float:
        """Authenticated round trip: an empty OTLP trace export (creates no telemetry) with the
        configured headers — ``POST {traces_url}`` with an empty protobuf body (HTTP) or
        ``TraceService/Export`` with an empty message (gRPC). Returns the latency in ms.

        :class:`AuthenticationError` on 401 / gRPC UNAUTHENTICATED,
        :class:`PermissionDeniedError` on 403, :class:`NotFoundError` for a wrong path.
        """
        self._check_open("ping")
        started = time.monotonic()
        if self.config.protocol == "grpc":
            grpc_call(
                self._grpc_channel(),
                "ping",
                GRPC_EXPORT_PATHS["traces"],
                b"",
                metadata=grpc_metadata(self.config.headers_for("traces")),
                timeout=self.config.request_timeout,
                retry=self.config.retry,
                known_secrets=self.config.known_secrets,
                cancel=cancel,
            )
        else:
            self._ping_response(
                self._transport.request(_ping_request(self.config, self._ping_path), cancel=cancel)
            )
        return (time.monotonic() - started) * 1000.0

    def _grpc_channel(self) -> grpc.Channel:
        with self._lock:
            if self._channel is None:
                self._channel = open_channel(str(self.config.grpc_url), self.config.tls)
            return self._channel

    def server_version(self, *, cancel: CancelToken | None = None) -> str:
        """Collector version from zpages ``/debug/servicez`` (works on stock stacks), else
        ``target_info`` at ``telemetry_url``; :class:`UnsupportedError` when neither answers."""
        self._check_open("server_version")
        return self._health.server_version(cancel=cancel)

    def _own(self, resource: _T, stop: Callable[[], Any]) -> _T:
        with self._lock:
            if not self._closed:
                self._owned.append(stop)
                return resource
        with contextlib.suppress(Exception):
            stop()
        raise _closed_error("client")

    def init_telemetry(self, **kwargs: Any) -> TelemetryHandle:
        """:func:`init_telemetry` with this client's config; :meth:`close` shuts it down."""
        handle = init_telemetry(self.config, **kwargs)
        return self._own(handle, handle.shutdown)

    def init_tracing(
        self, config: TracingConfig | None = None, **kwargs: Any
    ) -> CollectorTracingHandle:
        """:func:`init_tracing` with this client's config as defaults; :meth:`close` shuts it
        down."""
        handle = init_tracing(config, collector=self.config, **kwargs)
        return self._own(handle, handle.shutdown)

    def log_client(self, **kwargs: Any) -> BatchingLogClient:
        """An OTLP :class:`BatchingLogClient` (:func:`create_log_client`); :meth:`close`
        flushes and closes it."""
        client = create_log_client(self.config, **kwargs)
        return self._own(client, client.close)

    def log_exporter(self) -> OtlpLogExporter:
        """A bare :class:`OtlpLogExporter` for this config; :meth:`close` shuts it down."""
        exporter = OtlpLogExporter(self.config)
        return self._own(exporter, exporter.shutdown)

    def trace_query(self) -> CollectorTraceQueryClient:
        """Trace query client: the collector has none, every method raises
        :class:`UnsupportedError`."""
        return CollectorTraceQueryClient()

    def log_query(self) -> CollectorLogQueryClient:
        """Log query client: the collector has none, every method raises
        :class:`UnsupportedError`."""
        return CollectorLogQueryClient()

    def close(self) -> None:
        """Flush and shut down every handle, log client and exporter this client created, then
        release connections. Idempotent."""
        with self._lock:
            if self._closed:
                return
            self._closed = True
            owned, self._owned = self._owned, []
            channel, self._channel = self._channel, None
        for stop in owned:
            with contextlib.suppress(Exception):
                stop()
        if channel is not None:
            channel.close()
        self._transport.close()
        self._health.close()

    def __enter__(self) -> OtelCollectorClient:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()


class AsyncOtelCollectorClient(_ClientBase):
    """Asynchronous OpenTelemetry Collector client; use ``async with`` or call :meth:`aclose`.

    Cancel a call by cancelling its task: the SDK raises :class:`CancelledError`, which is
    also an :class:`asyncio.CancelledError`.
    """

    def __init__(
        self,
        config: OtelCollectorConfig | None = None,
        *,
        http_transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        super().__init__(config)
        self._transport = AsyncHttpTransport(
            self._transport_cfg,
            known_secrets=self.config.known_secrets,
            http_transport=http_transport,
        )
        self._health = AsyncCollectorHealthClient(self.config, http_transport=http_transport)
        self._channel: grpc.aio.Channel | None = None
        self._owned: list[Callable[[], Any]] = []

    @classmethod
    def from_env(
        cls,
        env: Mapping[str, str] | None = None,
        **overrides: Any,
    ) -> AsyncOtelCollectorClient:
        """See :meth:`OtelCollectorClient.from_env`."""
        return cls(OtelCollectorConfig.from_env(env, **overrides))

    @property
    def health_client(self) -> AsyncCollectorHealthClient:
        """The async health client."""
        return self._health

    async def request(self, req: Request) -> Response:
        """Send a raw request to the OTLP/HTTP receiver."""
        return await self._transport.request(req)

    async def health(self) -> HealthStatus:
        """See :meth:`OtelCollectorClient.health`."""
        self._check_open("health")
        return await self._health.health()

    async def wait_until_ready(
        self,
        timeout_s: float | timedelta = DEFAULT_READY_TIMEOUT_S,
        *,
        interval_s: float | timedelta = DEFAULT_READY_INTERVAL_S,
    ) -> HealthStatus:
        """See :meth:`OtelCollectorClient.wait_until_ready`."""
        self._check_open("wait_until_ready")
        return await self._health.wait_until_ready(timeout_s, interval_s=interval_s)

    async def ping(self) -> float:
        """See :meth:`OtelCollectorClient.ping`."""
        self._check_open("ping")
        started = time.monotonic()
        if self.config.protocol == "grpc":
            if self._channel is None:
                self._channel = open_aio_channel(str(self.config.grpc_url), self.config.tls)
            await grpc_call_async(
                self._channel,
                "ping",
                GRPC_EXPORT_PATHS["traces"],
                b"",
                metadata=grpc_metadata(self.config.headers_for("traces")),
                timeout=self.config.request_timeout,
                retry=self.config.retry,
                known_secrets=self.config.known_secrets,
            )
        else:
            self._ping_response(
                await self._transport.request(_ping_request(self.config, self._ping_path))
            )
        return (time.monotonic() - started) * 1000.0

    async def server_version(self) -> str:
        """See :meth:`OtelCollectorClient.server_version`."""
        self._check_open("server_version")
        return await self._health.server_version()

    def _own(self, resource: _T, stop: Callable[[], Any]) -> _T:
        self._owned.append(stop)
        return resource

    def _require_open(self) -> None:
        if self._closed:
            raise _closed_error("client")

    def init_telemetry(self, **kwargs: Any) -> TelemetryHandle:
        """:func:`init_telemetry` with this client's config; :meth:`aclose` shuts it down."""
        self._require_open()
        handle = init_telemetry(self.config, **kwargs)
        return self._own(handle, handle.shutdown)

    def init_tracing(
        self, config: TracingConfig | None = None, **kwargs: Any
    ) -> CollectorTracingHandle:
        """:func:`init_tracing` with this client's config as defaults; :meth:`aclose` shuts it
        down."""
        self._require_open()
        handle = init_tracing(config, collector=self.config, **kwargs)
        return self._own(handle, handle.shutdown)

    def log_client(self, **kwargs: Any) -> AsyncBatchingLogClient:
        """An OTLP :class:`AsyncBatchingLogClient` (:func:`create_async_log_client`); call it
        inside a running event loop. :meth:`aclose` flushes and closes it."""
        self._require_open()
        client = create_async_log_client(self.config, **kwargs)
        return self._own(client, client.close)

    def log_exporter(self) -> AsyncOtlpLogExporter:
        """A bare :class:`AsyncOtlpLogExporter`; :meth:`aclose` shuts it down."""
        self._require_open()
        exporter = AsyncOtlpLogExporter(self.config)
        return self._own(exporter, exporter.shutdown)

    def trace_query(self) -> AsyncCollectorTraceQueryClient:
        """Async trace query client: every method raises :class:`UnsupportedError`."""
        return AsyncCollectorTraceQueryClient()

    def log_query(self) -> AsyncCollectorLogQueryClient:
        """Async log query client: every method raises :class:`UnsupportedError`."""
        return AsyncCollectorLogQueryClient()

    async def aclose(self) -> None:
        """Flush and shut down everything this client created, then release connections.
        Idempotent. Blocking shutdowns (tracer/meter providers) run in a worker thread."""
        if self._closed:
            return
        self._closed = True
        owned, self._owned = self._owned, []
        channel, self._channel = self._channel, None
        for stop in owned:
            with contextlib.suppress(Exception):
                if inspect.iscoroutinefunction(stop):
                    await stop()
                else:
                    await asyncio.to_thread(stop)
        if channel is not None:
            await channel.close()
        await self._transport.aclose()
        await self._health.aclose()

    close = aclose

    async def __aenter__(self) -> AsyncOtelCollectorClient:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        await self.aclose()
