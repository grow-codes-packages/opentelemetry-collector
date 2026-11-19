"""One-call telemetry bootstrap: traces, logs and metrics exported over OTLP to the collector
with one shared resource (detected host/process/container/Kubernetes attributes).

Service-owned module.
"""

from __future__ import annotations

import concurrent.futures
import dataclasses
import threading
from collections.abc import Callable, Mapping, Sequence
from datetime import timedelta
from types import TracebackType
from typing import Any, Literal

from opentelemetry import trace as otel_trace
from opentelemetry.metrics import Meter, NoOpMeterProvider
from opentelemetry.sdk.metrics import MeterProvider as SdkMeterProvider
from opentelemetry.sdk.metrics.export import MetricExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import SpanProcessor, TracerProvider
from opentelemetry.sdk.trace.id_generator import IdGenerator

from ._core import TimeoutError, to_seconds
from ._logging import (
    AttrValue,
    BatchingLogClient,
    BatchOptions,
    LogExporter,
    Logger,
    LogInput,
    LogRecord,
    RedactionProcessor,
    Severity,
)
from ._tracing import PropagatorFactory, SamplerName, SpanBatchOptions
from .config import OtelCollectorConfig
from .logs import OtlpLogExporter, create_log_client
from .metrics import (
    RUNTIME_METER_NAME,
    MetricsApi,
    RuntimeMetricsHandle,
    create_meter_provider,
    register_runtime_metrics,
    set_global_meter_provider,
)
from .resource import ReadFile, detect_resource_attributes
from .setup import CollectorTracingHandle, init_tracing

__all__ = [
    "TelemetryHandle",
    "TelemetryLogsOptions",
    "TelemetryMetricsOptions",
    "TelemetryTracesOptions",
    "init_telemetry",
    "telemetry_resource_attributes",
]

_UNSET: Any = object()


@dataclasses.dataclass(frozen=True)
class TelemetryTracesOptions:
    """Trace settings of :func:`init_telemetry` (unset fields come from the collector config)."""

    sampler: SamplerName | None = None
    sampler_ratio: float | None = None
    propagators: Sequence[str] | None = None
    batch: SpanBatchOptions | None = None
    endpoint: str | None = None
    headers: Mapping[str, str] | None = None
    span_processors: Sequence[SpanProcessor] = ()
    propagator_factories: Mapping[str, PropagatorFactory] | None = None
    id_generator: IdGenerator | None = None


@dataclasses.dataclass(frozen=True)
class TelemetryLogsOptions:
    """Log settings of :func:`init_telemetry`.

    ``batch`` defaults to the config's ``logs_batch``; ``redaction`` to
    ``RedactionProcessor()`` (``None`` disables it); ``min_severity`` applies to
    :meth:`TelemetryHandle.logger`. ``exporter`` replaces the OTLP exporter (tests).
    """

    batch: BatchOptions | None = None
    redaction: RedactionProcessor | None = _UNSET
    min_severity: Severity = Severity.INFO
    exporter: LogExporter | None = None


@dataclasses.dataclass(frozen=True)
class TelemetryMetricsOptions:
    """Metric settings of :func:`init_telemetry` (defaults from the config).
    ``exporter`` replaces the OTLP exporter (tests)."""

    export_interval_s: float | timedelta | None = None
    runtime_metrics: bool | None = None
    exporter: MetricExporter | None = None


def telemetry_resource_attributes(
    config: OtelCollectorConfig,
    *,
    detectors: Sequence[str] | None = None,
    env: Mapping[str, str] | None = None,
    read_file: ReadFile | None = None,
) -> dict[str, Any]:
    """Resource attributes for a config: detected < config attributes < service fields
    (``service.name``, ``service.version``, ``deployment.environment[.name]``)."""
    attrs: dict[str, Any] = detect_resource_attributes(
        config.resource_detectors if detectors is None else detectors,
        env=env,
        read_file=read_file,
    )
    attrs.update(config.resource_attributes)
    attrs["service.name"] = config.service_name
    if config.service_version:
        attrs["service.version"] = config.service_version
    if config.environment:
        attrs["deployment.environment.name"] = config.environment
        attrs["deployment.environment"] = config.environment
    return attrs


class _DropSink:
    def send(self, record: LogRecord | LogInput) -> None:
        """Discard the record (logs disabled)."""


_DROP_SINK = _DropSink()


def _enabled(value: Any, fallback: bool, factory: Callable[[], Any]) -> Any:
    if value is False:
        return None
    if value is None:
        return factory() if fallback else None
    return factory() if value is True else value


class TelemetryHandle(MetricsApi):
    """A running telemetry setup (all three signals); see :func:`init_telemetry`.

    Instrument helpers (``meter``, ``counter``, ``up_down_counter``, ``histogram``,
    ``gauge``) create instruments on this handle's meter provider (no-op when metrics are
    disabled).
    """

    def __init__(
        self,
        *,
        config: OtelCollectorConfig,
        resource_attributes: Mapping[str, Any],
        resource: Resource,
        tracing: CollectorTracingHandle | None,
        log_client: BatchingLogClient | None,
        meter_provider: SdkMeterProvider | None,
        runtime: RuntimeMetricsHandle | None,
        undo_global_meter: Callable[[], None],
        min_severity: Severity,
    ) -> None:
        self._meter_source = meter_provider or NoOpMeterProvider()
        super().__init__(self._get_meter)
        #: The collector config in use.
        self.config = config
        #: Attributes of the shared resource.
        self.resource_attributes: Mapping[str, Any] = dict(resource_attributes)
        #: The shared resource (with the SDK default attributes).
        self.resource = resource
        #: The tracing handle, when traces are enabled.
        self.tracing = tracing
        #: The OTLP log client, when logs are enabled.
        self.log_client = log_client
        #: The meter provider, when metrics are enabled.
        self.meter_provider = meter_provider
        self._runtime = runtime
        self._undo_global_meter = undo_global_meter
        self._min_severity = min_severity
        self._lock = threading.Lock()
        self._shut_down = False

    def _get_meter(self, name: str, version: str | None) -> Meter:
        return self._meter_source.get_meter(name, version)

    @property
    def tracer_provider(self) -> TracerProvider | None:
        """The tracer provider, when traces are enabled."""
        return self.tracing.tracer_provider if self.tracing is not None else None

    def tracer(self, name: str | None = None, version: str | None = None) -> otel_trace.Tracer:
        """A tracer (non-recording when traces are disabled); ``name`` defaults to the service
        name."""
        scope = name or str(self.config.service_name)
        if self.tracing is None:
            return otel_trace.NoOpTracer()
        return self.tracing.tracer(scope, version)

    def logger(
        self, name: str | None = None, attributes: Mapping[str, AttrValue] | None = None
    ) -> Logger:
        """A :class:`Logger` writing to :attr:`log_client` (drops records when logs are
        disabled)."""
        if self.log_client is not None:
            return self.log_client.logger(name, attributes, min_severity=self._min_severity)
        return Logger(_DROP_SINK, name=name, attributes=attributes, min_severity=self._min_severity)

    def force_flush(self, timeout_s: float | timedelta = 30.0) -> None:
        """Export everything buffered in every enabled signal; :class:`TimeoutError` when it
        does not finish within ``timeout_s``."""
        seconds = to_seconds(timeout_s)
        tasks: list[Callable[[], Any]] = []
        if self.tracing is not None:
            tracing = self.tracing
            tasks.append(lambda: tracing.force_flush(seconds))
        if self.log_client is not None:
            log_client = self.log_client
            tasks.append(lambda: log_client.flush(seconds))
        if self.meter_provider is not None:
            provider = self.meter_provider

            def flush_metrics() -> None:
                if not provider.force_flush(seconds * 1000.0):
                    raise TimeoutError("telemetry.force_flush: metrics timed out")

            tasks.append(flush_metrics)
        if not tasks:
            return
        pool = concurrent.futures.ThreadPoolExecutor(
            max_workers=len(tasks), thread_name_prefix="grow-otelcol-flush"
        )
        try:
            futures = [pool.submit(t) for t in tasks]
            done, pending = concurrent.futures.wait(futures, timeout=seconds)
            if pending:
                raise TimeoutError(f"telemetry.force_flush: timed out after {seconds:g}s")
            for future in done:
                future.result()
        finally:
            pool.shutdown(wait=False)

    def shutdown(self) -> None:
        """Flush and shut every provider down; idempotent. Unregisters the global meter
        provider of the module-level helpers. The first failure is raised after all
        providers were shut down."""
        with self._lock:
            if self._shut_down:
                return
            self._shut_down = True
        errors: list[BaseException] = []
        if self._runtime is not None:
            self._runtime.stop()
        for step in (
            self.tracing.shutdown if self.tracing is not None else None,
            self.log_client.close if self.log_client is not None else None,
            self.meter_provider.shutdown if self.meter_provider is not None else None,
        ):
            if step is None:
                continue
            try:
                step()
            except Exception as exc:  # noqa: BLE001 - shut everything down, raise the first
                errors.append(exc)
        self._undo_global_meter()
        if errors:
            raise errors[0]

    #: Alias so a handle can live in a client's resource registry.
    stop = shutdown

    @property
    def is_shut_down(self) -> bool:
        """Whether :meth:`shutdown` was called."""
        return self._shut_down

    def __enter__(self) -> TelemetryHandle:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.shutdown()


def init_telemetry(
    config: OtelCollectorConfig | None = None,
    *,
    env: Mapping[str, str] | Literal[False] | None = None,
    traces: bool | TelemetryTracesOptions | None = None,
    logs: bool | TelemetryLogsOptions | None = None,
    metrics: bool | TelemetryMetricsOptions | None = None,
    set_global: bool = True,
    detectors: Sequence[str] | None = None,
    detect_env: Mapping[str, str] | None = None,
    read_file: ReadFile | None = None,
) -> TelemetryHandle:
    """One-call OpenTelemetry bootstrap for every Grow stack (the platform deploys the
    collector with each stack).

    Traces (:func:`init_tracing`), logs (:class:`OtlpLogExporter` + ``BatchingLogClient``) and
    metrics (``MeterProvider`` + OTLP exporter) share one resource: detected attributes
    (``resource_detectors``) < config ``resource_attributes`` < service fields. Signals follow
    the config (``traces``/``logs``/``metrics``, ``OTEL_SDK_DISABLED``,
    ``OTEL_<SIGNAL>_EXPORTER=none``) unless ``traces``/``logs``/``metrics`` turn them on
    (``True`` or options) or off (``False``). ``config`` defaults to
    :meth:`OtelCollectorConfig.from_env` over ``env`` (``env=False`` ignores the environment).
    With ``set_global`` the tracer provider, propagator and meter provider become the globals
    used by ``with_span``, ``counter()`` … Exporter failures never raise at the caller.
    """
    if config is not None:
        cfg = config.validate()
    elif env is False:
        cfg = OtelCollectorConfig().validate()
    else:
        cfg = OtelCollectorConfig.from_env(env)
    attributes = telemetry_resource_attributes(
        cfg, detectors=detectors, env=detect_env, read_file=read_file
    )
    resource = Resource.create(attributes)
    traces_opts: TelemetryTracesOptions | None = _enabled(
        traces, cfg.traces, TelemetryTracesOptions
    )
    logs_opts: TelemetryLogsOptions | None = _enabled(logs, cfg.logs, TelemetryLogsOptions)
    metrics_opts: TelemetryMetricsOptions | None = _enabled(
        metrics, cfg.metrics, TelemetryMetricsOptions
    )

    tracing: CollectorTracingHandle | None = None
    log_client: BatchingLogClient | None = None
    provider: SdkMeterProvider | None = None
    runtime: RuntimeMetricsHandle | None = None
    undo: Callable[[], None] = lambda: None  # noqa: E731
    try:
        if traces_opts is not None:
            overrides = {
                k: getattr(traces_opts, k)
                for k in ("sampler", "sampler_ratio", "propagators", "batch", "endpoint", "headers")
                if getattr(traces_opts, k) is not None
            }
            tracing = init_tracing(
                collector=cfg,
                resource_attributes=attributes,
                set_global=set_global,
                span_processors=traces_opts.span_processors,
                propagator_factories=traces_opts.propagator_factories,
                id_generator=traces_opts.id_generator,
                **overrides,
            )
        if logs_opts is not None:
            exporter = logs_opts.exporter or OtlpLogExporter(cfg)
            kwargs: dict[str, Any] = {}
            if logs_opts.redaction is not _UNSET:
                kwargs["redaction"] = logs_opts.redaction
            log_client = create_log_client(
                cfg,
                options=logs_opts.batch,
                resource=attributes,
                exporter=exporter,
                **kwargs,
            )
        if metrics_opts is not None:
            interval = metrics_opts.export_interval_s
            provider = create_meter_provider(
                cfg,
                resource=resource,
                exporter=metrics_opts.exporter,
                export_interval_s=None if interval is None else to_seconds(interval),
            )
            if set_global:
                undo = set_global_meter_provider(provider)
            runtime_on = (
                cfg.runtime_metrics
                if metrics_opts.runtime_metrics is None
                else metrics_opts.runtime_metrics
            )
            if runtime_on:
                runtime = register_runtime_metrics(provider.get_meter(RUNTIME_METER_NAME))
    except BaseException:
        undo()
        for closer in (
            tracing.shutdown if tracing is not None else None,
            log_client.close if log_client is not None else None,
            provider.shutdown if provider is not None else None,
        ):
            if closer is not None:
                closer()
        raise
    return TelemetryHandle(
        config=cfg,
        resource_attributes=attributes,
        resource=resource,
        tracing=tracing,
        log_client=log_client,
        meter_provider=provider,
        runtime=runtime,
        undo_global_meter=undo,
        min_severity=logs_opts.min_severity if logs_opts is not None else Severity.INFO,
    )
