"""OTLP log export: :class:`OtlpLogExporter` / :class:`AsyncOtlpLogExporter` (SPEC §4
``LogExporter``) and the ``create_log_client`` helpers that wrap them in the template batching
log clients.

Records are encoded as an OTLP ``ExportLogsServiceRequest`` (binary protobuf from
``opentelemetry-proto``) and sent with ``POST {logs_url}`` (``application/x-protobuf``) or the
gRPC ``LogsService/Export`` method. Service-owned module.
"""

from __future__ import annotations

import asyncio
import gzip as _gzip
import json
import threading
from collections.abc import Mapping, Sequence
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any

from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import (
    ExportLogsServiceRequest,
    ExportLogsServiceResponse,
)
from opentelemetry.proto.common.v1.common_pb2 import (
    AnyValue,
    ArrayValue,
    InstrumentationScope,
    KeyValue,
    KeyValueList,
)
from opentelemetry.proto.logs.v1.logs_pb2 import LogRecord as PbLogRecord
from opentelemetry.proto.logs.v1.logs_pb2 import ResourceLogs, ScopeLogs
from opentelemetry.proto.resource.v1.resource_pb2 import Resource as PbResource

from ._core import (
    AsyncHttpTransport,
    BaseConfig,
    CancelledError,
    HttpTransport,
    Request,
    Response,
    ServiceError,
    ValidationError,
)
from ._logging import (
    AsyncBatchingLogClient,
    AsyncLogExporter,
    AttrValue,
    BatchingLogClient,
    BatchOptions,
    ExportResult,
    LogExporter,
    LogRecord,
    RedactionProcessor,
)
from ._version import __version__
from .config import OtelCollectorConfig
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

if TYPE_CHECKING:
    import grpc
    import grpc.aio

__all__ = [
    "LOG_SCOPE_NAME",
    "AsyncOtlpLogExporter",
    "OtlpLogExporter",
    "create_async_log_client",
    "create_log_client",
    "encode_log_records",
    "to_any_value",
]

#: Instrumentation scope name of records exported by :class:`OtlpLogExporter`.
LOG_SCOPE_NAME = "grow_opentelemetry_collector"

_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
_INT64_MIN, _INT64_MAX = -(2**63), 2**63 - 1
_UNSET: Any = object()


def to_any_value(value: Any) -> AnyValue:
    """OTLP ``AnyValue`` for an attribute value (scalars, lists, nested mappings, bytes)."""
    if isinstance(value, bool):
        return AnyValue(bool_value=value)
    if isinstance(value, int):
        if _INT64_MIN <= value <= _INT64_MAX:
            return AnyValue(int_value=value)
        return AnyValue(string_value=str(value))
    if isinstance(value, float):
        return AnyValue(double_value=value)
    if isinstance(value, str):
        return AnyValue(string_value=value)
    if isinstance(value, (bytes, bytearray)):
        return AnyValue(bytes_value=bytes(value))
    if isinstance(value, Mapping):
        return AnyValue(kvlist_value=KeyValueList(values=_key_values(value)))
    if isinstance(value, (list, tuple)):
        return AnyValue(array_value=ArrayValue(values=[to_any_value(v) for v in value]))
    return AnyValue(string_value=str(value))


def _key_values(attributes: Mapping[str, Any]) -> list[KeyValue]:
    return [KeyValue(key=str(k), value=to_any_value(v)) for k, v in attributes.items()]


def _nanos(value: datetime) -> int:
    if value.tzinfo is None:
        value = value.replace(tzinfo=timezone.utc)
    delta = value - _EPOCH
    return (delta.days * 86_400 + delta.seconds) * 1_000_000_000 + delta.microseconds * 1000


def _id_bytes(value: str | None, length: int) -> bytes:
    if not value:
        return b""
    try:
        raw = bytes.fromhex(value)
    except ValueError:
        return b""
    return raw if len(raw) == length else b""


def _pb_record(record: LogRecord) -> PbLogRecord:
    observed = record.observed_timestamp or record.timestamp
    out = PbLogRecord(
        time_unix_nano=_nanos(record.timestamp),
        observed_time_unix_nano=_nanos(observed),
        severity_number=int(record.severity),  # type: ignore[arg-type]
        severity_text=record.severity_text,
        body=AnyValue(string_value=record.body),
        attributes=_key_values(record.attributes),
        trace_id=_id_bytes(record.trace_id, 16),
        span_id=_id_bytes(record.span_id, 8),
    )
    if record.trace_flags is not None:
        out.flags = record.trace_flags & 0xFF
    return out


def encode_log_records(records: Sequence[LogRecord]) -> ExportLogsServiceRequest:
    """One ``ExportLogsServiceRequest`` for ``records`` (one ``ResourceLogs`` per distinct
    resource, scope :data:`LOG_SCOPE_NAME`). Timestamps become Unix nanoseconds, severity the
    OTel number, trace and span ids binary."""
    groups: dict[str, tuple[Mapping[str, AttrValue], list[PbLogRecord]]] = {}
    for record in records:
        key = json.dumps(record.resource, sort_keys=True, default=str)
        if key not in groups:
            groups[key] = (record.resource, [])
        groups[key][1].append(_pb_record(record))
    scope = InstrumentationScope(name=LOG_SCOPE_NAME, version=__version__)
    return ExportLogsServiceRequest(
        resource_logs=[
            ResourceLogs(
                resource=PbResource(attributes=_key_values(resource)),
                scope_logs=[ScopeLogs(scope=scope, log_records=pb_records)],
            )
            for resource, pb_records in groups.values()
        ]
    )


def _result(n: int, response: ExportLogsServiceResponse, retries: int) -> ExportResult:
    partial = response.partial_success
    rejected = min(n, max(0, int(partial.rejected_log_records)))
    if rejected == 0:
        return ExportResult(accepted=n, retries=retries)
    detail = f" ({partial.error_message[:200]})" if partial.error_message else ""
    return ExportResult(
        accepted=n - rejected,
        rejected=rejected,
        error=ValidationError(
            f"logs.export: the collector rejected {rejected} of {n} records{detail}",
            retryable=False,
        ),
        retries=retries,
    )


def _decode(content: bytes) -> ExportLogsServiceResponse:
    try:
        response: ExportLogsServiceResponse = ExportLogsServiceResponse.FromString(content)
    except Exception:  # noqa: BLE001 - a malformed success body still means "accepted"
        return ExportLogsServiceResponse()
    return response


class _ExporterBase:
    def __init__(self, config: OtelCollectorConfig | None) -> None:
        cfg = (config if config is not None else OtelCollectorConfig.from_env()).validate()
        self.config = cfg
        #: Endpoint records are sent to (``logs_url`` for HTTP, ``grpc_url`` for gRPC).
        self.endpoint = cfg.endpoint_for("logs")
        self._headers = cfg.headers_for("logs")
        self._base, self._path = split_url(self.endpoint)
        self._shutdown = False

    def _transport_config(self) -> BaseConfig:
        cfg = self.config
        return BaseConfig(
            url=self._base,
            connect_timeout=min(cfg.connect_timeout, cfg.export_timeout),
            request_timeout=min(cfg.request_timeout, cfg.export_timeout),
            total_timeout=cfg.export_timeout,
            retry=cfg.retry,
            tls=cfg.tls,
            proxy=cfg.proxy,
            headers=self._headers,
            user_agent_suffix=cfg.user_agent_suffix,
            hooks=cfg.hooks,
            logger=cfg.logger,
        )

    def _http_request(self, payload: bytes, gzip: bool, timeout_s: float) -> Request:
        headers = {"Content-Type": "application/x-protobuf", "Accept": "application/x-protobuf"}
        if gzip and self.config.compression == "gzip":
            payload = _gzip.compress(payload)
            headers["Content-Encoding"] = "gzip"
        return Request(
            "POST",
            self._path,
            operation="logs.export",
            content=payload,
            headers=headers,
            timeout=min(timeout_s, self.config.export_timeout),
            raise_for_status=False,
        )

    def _from_response(self, n: int, resp: Response) -> ExportResult:
        retries = max(0, resp.attempts - 1)
        if resp.ok:
            return _result(n, _decode(resp.content), retries)
        error = error_for_http_response("logs.export", resp, self.config.known_secrets())
        return ExportResult(accepted=0, rejected=n, error=error, retries=retries)

    def _closed(self, n: int) -> ExportResult:
        return ExportResult(
            accepted=0,
            rejected=n,
            error=ValidationError("logs.export: exporter is shut down", retryable=False),
        )


class OtlpLogExporter(_ExporterBase):
    """Synchronous :class:`~grow_opentelemetry_collector.LogExporter` sending OTLP logs to the
    collector (``http/protobuf`` or ``grpc`` per the config).

    HTTP uses the SDK transport (retries 429 and connection failures within
    ``export_timeout``; other retryable errors are retried by the batch processor); gRPC
    retries UNAVAILABLE and RESOURCE_EXHAUSTED. A partial success reports ``rejected`` and a
    :class:`ValidationError`. Errors are returned in the result, never raised.
    """

    def __init__(
        self, config: OtelCollectorConfig | None = None, *, http_transport: Any = None
    ) -> None:
        super().__init__(config)
        self._lock = threading.Lock()
        self._transport: HttpTransport | None = None
        self._channel: grpc.Channel | None = None
        self._http_transport = http_transport

    def export(
        self, records: Sequence[LogRecord], *, gzip: bool = True, timeout_s: float = 30.0
    ) -> ExportResult:
        """Export ``records`` in one OTLP request and report accepted/rejected counts."""
        n = len(records)
        if n == 0:
            return ExportResult(accepted=0)
        if self._shutdown:
            return self._closed(n)
        try:
            payload = encode_log_records(records).SerializeToString()
            if self.config.protocol == "grpc":
                content = grpc_call(
                    self._grpc_channel(),
                    "logs.export",
                    GRPC_EXPORT_PATHS["logs"],
                    payload,
                    metadata=grpc_metadata(self._headers),
                    timeout=min(timeout_s, self.config.export_timeout),
                    retry=self.config.retry,
                    gzip=gzip and self.config.compression == "gzip",
                    known_secrets=self.config.known_secrets,
                )
                return _result(n, _decode(content), 0)
            resp = self._http().request(self._http_request(payload, gzip, timeout_s))
            return self._from_response(n, resp)
        except ServiceError as err:
            return ExportResult(accepted=0, rejected=n, error=err)

    def _http(self) -> HttpTransport:
        with self._lock:
            if self._transport is None:
                self._transport = HttpTransport(
                    self._transport_config(),
                    known_secrets=self.config.known_secrets,
                    http_transport=self._http_transport,
                )
            return self._transport

    def _grpc_channel(self) -> grpc.Channel:
        with self._lock:
            if self._channel is None:
                self._channel = open_channel(self.endpoint, self.config.tls)
            return self._channel

    def shutdown(self) -> None:
        """Release connections; later exports fail with a ``ValidationError``. Idempotent."""
        with self._lock:
            self._shutdown = True
            transport, self._transport = self._transport, None
            channel, self._channel = self._channel, None
        if transport is not None:
            transport.close()
        if channel is not None:
            channel.close()

    #: Alias so the exporter can live in a client's resource registry.
    stop = shutdown


class AsyncOtlpLogExporter(_ExporterBase):
    """Asynchronous :class:`OtlpLogExporter` (an ``AsyncLogExporter``)."""

    def __init__(
        self, config: OtelCollectorConfig | None = None, *, http_transport: Any = None
    ) -> None:
        super().__init__(config)
        self._transport: AsyncHttpTransport | None = None
        self._channel: grpc.aio.Channel | None = None
        self._http_transport = http_transport

    async def export(
        self, records: Sequence[LogRecord], *, gzip: bool = True, timeout_s: float = 30.0
    ) -> ExportResult:
        """See :meth:`OtlpLogExporter.export`; task cancellation raises
        :class:`CancelledError`."""
        n = len(records)
        if n == 0:
            return ExportResult(accepted=0)
        if self._shutdown:
            return self._closed(n)
        try:
            payload = encode_log_records(records).SerializeToString()
            if self.config.protocol == "grpc":
                if self._channel is None:
                    self._channel = open_aio_channel(self.endpoint, self.config.tls)
                content = await grpc_call_async(
                    self._channel,
                    "logs.export",
                    GRPC_EXPORT_PATHS["logs"],
                    payload,
                    metadata=grpc_metadata(self._headers),
                    timeout=min(timeout_s, self.config.export_timeout),
                    retry=self.config.retry,
                    gzip=gzip and self.config.compression == "gzip",
                    known_secrets=self.config.known_secrets,
                )
                return _result(n, _decode(content), 0)
            if self._transport is None:
                self._transport = AsyncHttpTransport(
                    self._transport_config(),
                    known_secrets=self.config.known_secrets,
                    http_transport=self._http_transport,
                )
            resp = await self._transport.request(self._http_request(payload, gzip, timeout_s))
            return self._from_response(n, resp)
        except (CancelledError, asyncio.CancelledError):
            raise
        except ServiceError as err:
            return ExportResult(accepted=0, rejected=n, error=err)

    async def shutdown(self) -> None:
        """Release connections; later exports fail with a ``ValidationError``. Idempotent."""
        self._shutdown = True
        transport, self._transport = self._transport, None
        channel, self._channel = self._channel, None
        if transport is not None:
            await transport.aclose()
        if channel is not None:
            await channel.close()

    aclose = shutdown


def _default_resource(cfg: OtelCollectorConfig) -> dict[str, AttrValue]:
    resource: dict[str, AttrValue] = dict(cfg.resource_attributes)
    resource["service.name"] = str(cfg.service_name)
    if cfg.service_version:
        resource["service.version"] = cfg.service_version
    if cfg.environment:
        resource["deployment.environment.name"] = cfg.environment
    return resource


def _batch_options(cfg: OtelCollectorConfig, options: BatchOptions | None) -> BatchOptions:
    return options if options is not None else BatchOptions(**dict(cfg.logs_batch))


def create_log_client(
    config: OtelCollectorConfig | None = None,
    *,
    options: BatchOptions | None = None,
    resource: Mapping[str, AttrValue] | None = None,
    redaction: RedactionProcessor | None = _UNSET,
    correlate: bool = True,
    exporter: LogExporter | None = None,
) -> BatchingLogClient:
    """A :class:`BatchingLogClient` exporting OTLP logs to the collector.

    ``resource`` defaults to the configured resource attributes plus ``service.name``,
    ``service.version`` and ``deployment.environment.name``; ``options`` default to the
    configured ``logs_batch`` (``OTEL_BLRP_*``). ``redaction`` defaults to
    ``RedactionProcessor()`` (``None`` disables it). ``config`` defaults to
    :meth:`OtelCollectorConfig.from_env`.
    """
    cfg = (config if config is not None else OtelCollectorConfig.from_env()).validate()
    kwargs: dict[str, Any] = {} if redaction is _UNSET else {"redaction": redaction}
    return BatchingLogClient(
        exporter if exporter is not None else OtlpLogExporter(cfg),
        options=_batch_options(cfg, options),
        resource=resource if resource is not None else _default_resource(cfg),
        correlate=correlate,
        **kwargs,
    )


def create_async_log_client(
    config: OtelCollectorConfig | None = None,
    *,
    options: BatchOptions | None = None,
    resource: Mapping[str, AttrValue] | None = None,
    redaction: RedactionProcessor | None = _UNSET,
    correlate: bool = True,
    exporter: AsyncLogExporter | None = None,
) -> AsyncBatchingLogClient:
    """Async :func:`create_log_client` (an :class:`AsyncBatchingLogClient` over
    :class:`AsyncOtlpLogExporter`); create it inside a running event loop."""
    cfg = (config if config is not None else OtelCollectorConfig.from_env()).validate()
    kwargs: dict[str, Any] = {} if redaction is _UNSET else {"redaction": redaction}
    return AsyncBatchingLogClient(
        exporter if exporter is not None else AsyncOtlpLogExporter(cfg),
        options=_batch_options(cfg, options),
        resource=resource if resource is not None else _default_resource(cfg),
        correlate=correlate,
        **kwargs,
    )
