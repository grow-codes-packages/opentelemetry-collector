"""OTLP metrics: a ``MeterProvider`` with the official OTLP metric exporter (periodic reader),
``meter``/``counter``/``up_down_counter``/``histogram``/``gauge`` helpers and optional Python
runtime metrics. Service-owned module.
"""

from __future__ import annotations

import gc
import os
import resource as _resource
import sys
import threading
import time
from collections.abc import Callable, Iterable, Sequence

from opentelemetry import metrics as otel_metrics
from opentelemetry.metrics import (
    CallbackOptions,
    Counter,
    Histogram,
    Meter,
    MeterProvider,
    Observation,
    UpDownCounter,
)
from opentelemetry.metrics import _Gauge as Gauge
from opentelemetry.sdk.metrics import MeterProvider as SdkMeterProvider
from opentelemetry.sdk.metrics.export import MetricExporter, PeriodicExportingMetricReader
from opentelemetry.sdk.resources import Resource

from ._core import ConfigError
from ._tracing import INSTRUMENTATION_NAME
from .config import OtelCollectorConfig
from .otlp import grpc_credentials, grpc_metadata, grpc_module
from .setup import http_exporter_kwargs

__all__ = [
    "DEFAULT_METER_NAME",
    "RUNTIME_METER_NAME",
    "Gauge",
    "MetricsApi",
    "RuntimeMetricsHandle",
    "counter",
    "create_meter_provider",
    "create_metric_exporter",
    "gauge",
    "histogram",
    "meter",
    "register_runtime_metrics",
    "set_global_meter_provider",
    "up_down_counter",
]

#: Meter name used by the helpers when none is given.
DEFAULT_METER_NAME = INSTRUMENTATION_NAME
#: Meter name of the runtime metrics.
RUNTIME_METER_NAME = f"{INSTRUMENTATION_NAME}.runtime"

_GLOBAL_LOCK = threading.Lock()
_GLOBAL: list[MeterProvider] = []


class MetricsApi:
    """Instrument helpers bound to a meter source (a provider, or the global one)."""

    def __init__(self, get_meter: Callable[[str, str | None], Meter]) -> None:
        self._get_meter = get_meter

    def meter(self, name: str = DEFAULT_METER_NAME, version: str | None = None) -> Meter:
        """A meter from this source."""
        return self._get_meter(name, version)

    def counter(
        self, name: str, *, unit: str = "", description: str = "", meter: str | None = None
    ) -> Counter:
        """Monotonic counter (OTLP ``sum``, ``isMonotonic: true``, cumulative)."""
        return self.meter(meter or DEFAULT_METER_NAME).create_counter(name, unit, description)

    def up_down_counter(
        self, name: str, *, unit: str = "", description: str = "", meter: str | None = None
    ) -> UpDownCounter:
        """Up/down counter (OTLP ``sum``, non-monotonic)."""
        return self.meter(meter or DEFAULT_METER_NAME).create_up_down_counter(
            name, unit, description
        )

    def histogram(
        self,
        name: str,
        *,
        unit: str = "",
        description: str = "",
        boundaries: Sequence[float] | None = None,
        meter: str | None = None,
    ) -> Histogram:
        """Histogram with optional explicit bucket ``boundaries`` (ascending; SDK advice)."""
        return self.meter(meter or DEFAULT_METER_NAME).create_histogram(
            name,
            unit,
            description,
            explicit_bucket_boundaries_advisory=list(boundaries) if boundaries else None,
        )

    def gauge(
        self, name: str, *, unit: str = "", description: str = "", meter: str | None = None
    ) -> Gauge:
        """Synchronous gauge (OTLP ``gauge``, last value)."""
        return self.meter(meter or DEFAULT_METER_NAME).create_gauge(name, unit, description)


def _global_meter(name: str, version: str | None) -> Meter:
    with _GLOBAL_LOCK:
        provider = _GLOBAL[-1] if _GLOBAL else None
    if provider is not None:
        return provider.get_meter(name, version)
    return otel_metrics.get_meter(name, version or "")


def set_global_meter_provider(provider: MeterProvider | None) -> Callable[[], None]:
    """Make ``provider`` the target of the module-level helpers (``counter()`` …) and, when no
    global provider was set yet, the OpenTelemetry global. Returns a function that undoes the
    helper registration (the OpenTelemetry global can be set only once per process)."""
    if provider is None:
        return lambda: None
    with _GLOBAL_LOCK:
        _GLOBAL.append(provider)
    otel_metrics.set_meter_provider(provider)

    def undo() -> None:
        with _GLOBAL_LOCK:
            if provider in _GLOBAL:
                _GLOBAL.remove(provider)

    return undo


_global_api = MetricsApi(_global_meter)


def meter(name: str = DEFAULT_METER_NAME, version: str | None = None) -> Meter:
    """A meter from the global provider (set by ``init_telemetry`` unless
    ``set_global=False``)."""
    return _global_api.meter(name, version)


def counter(
    name: str, *, unit: str = "", description: str = "", meter: str | None = None
) -> Counter:
    """Monotonic counter on the global provider."""
    return _global_api.counter(name, unit=unit, description=description, meter=meter)


def up_down_counter(
    name: str, *, unit: str = "", description: str = "", meter: str | None = None
) -> UpDownCounter:
    """Up/down counter on the global provider."""
    return _global_api.up_down_counter(name, unit=unit, description=description, meter=meter)


def histogram(
    name: str,
    *,
    unit: str = "",
    description: str = "",
    boundaries: Sequence[float] | None = None,
    meter: str | None = None,
) -> Histogram:
    """Histogram on the global provider."""
    return _global_api.histogram(
        name, unit=unit, description=description, boundaries=boundaries, meter=meter
    )


def gauge(name: str, *, unit: str = "", description: str = "", meter: str | None = None) -> Gauge:
    """Synchronous gauge on the global provider."""
    return _global_api.gauge(name, unit=unit, description=description, meter=meter)


def create_metric_exporter(config: OtelCollectorConfig) -> MetricExporter:
    """The official OTLP metric exporter for a collector config: ``http/protobuf`` →
    ``opentelemetry-exporter-otlp-proto-http`` (``{metrics_url}``), ``grpc`` →
    ``opentelemetry-exporter-otlp-proto-grpc``. Cumulative temporality."""
    endpoint = config.endpoint_for("metrics")
    headers = config.headers_for("metrics")
    gzip = config.compression == "gzip"
    exporter: MetricExporter
    if config.protocol == "grpc":
        grpc = grpc_module()
        from opentelemetry.exporter.otlp.proto.grpc.metric_exporter import (  # noqa: PLC0415
            OTLPMetricExporter as GrpcMetricExporter,
        )

        credentials = grpc_credentials(endpoint, config.tls)
        exporter = GrpcMetricExporter(
            endpoint=endpoint,
            insecure=credentials is None,
            credentials=credentials,
            headers=dict(grpc_metadata(headers)),
            timeout=config.export_timeout,
            compression=grpc.Compression.Gzip if gzip else grpc.Compression.NoCompression,
        )
        return exporter
    from opentelemetry.exporter.otlp.proto.http import Compression  # noqa: PLC0415
    from opentelemetry.exporter.otlp.proto.http.metric_exporter import (  # noqa: PLC0415
        OTLPMetricExporter as HttpMetricExporter,
    )

    exporter = HttpMetricExporter(
        endpoint=endpoint,
        headers=headers,
        timeout=config.export_timeout,
        compression=Compression.Gzip if gzip else Compression.NoCompression,
        **http_exporter_kwargs(config.tls),
    )
    return exporter


def create_meter_provider(
    config: OtelCollectorConfig,
    *,
    resource: Resource,
    exporter: MetricExporter | None = None,
    export_interval_s: float | None = None,
) -> SdkMeterProvider:
    """``MeterProvider`` with a periodic reader (``metric_export_interval``) around the OTLP
    exporter (or ``exporter``)."""
    interval = config.metric_export_interval if export_interval_s is None else export_interval_s
    if not interval > 0:
        raise ConfigError("metrics: export_interval_s must be positive")
    reader = PeriodicExportingMetricReader(
        exporter if exporter is not None else create_metric_exporter(config),
        export_interval_millis=interval * 1000.0,
        export_timeout_millis=min(config.export_timeout, interval) * 1000.0,
    )
    return SdkMeterProvider(metric_readers=[reader], resource=resource, shutdown_on_exit=False)


def _rss_bytes() -> int:
    try:
        with open("/proc/self/statm", encoding="ascii") as fh:
            return int(fh.read().split()[1]) * os.sysconf("SC_PAGE_SIZE")
    except (OSError, ValueError, IndexError):
        peak = _resource.getrusage(_resource.RUSAGE_SELF).ru_maxrss
        return peak if sys.platform == "darwin" else peak * 1024


class RuntimeMetricsHandle:
    """Stops the runtime metrics callbacks (:func:`register_runtime_metrics`)."""

    def __init__(self) -> None:
        self._stopped = threading.Event()

    @property
    def stopped(self) -> bool:
        """Whether :meth:`stop` was called."""
        return self._stopped.is_set()

    def stop(self) -> None:
        """Stop reporting (idempotent)."""
        self._stopped.set()


def register_runtime_metrics(meter_instance: Meter) -> RuntimeMetricsHandle:
    """Register Python runtime metrics on ``meter_instance``: ``process.memory.usage`` (RSS,
    By), ``process.cpu.time`` (s, ``cpu.mode`` user/system), ``process.thread.count``,
    ``cpython.gc.collections`` (per ``cpython.gc.generation``) and ``process.uptime`` (s)."""
    handle = RuntimeMetricsHandle()

    def guard(
        fn: Callable[[], Iterable[Observation]],
    ) -> Callable[[CallbackOptions], Iterable[Observation]]:
        def callback(_options: CallbackOptions) -> Iterable[Observation]:
            return [] if handle.stopped else list(fn())

        return callback

    def cpu() -> Iterable[Observation]:
        times = os.times()
        yield Observation(times.user, {"cpu.mode": "user"})
        yield Observation(times.system, {"cpu.mode": "system"})

    def collections() -> Iterable[Observation]:
        for generation, stats in enumerate(gc.get_stats()):
            yield Observation(stats.get("collections", 0), {"cpython.gc.generation": generation})

    meter_instance.create_observable_gauge(
        "process.memory.usage",
        [guard(lambda: [Observation(_rss_bytes())])],
        unit="By",
        description="Resident set size of the process.",
    )
    meter_instance.create_observable_counter(
        "process.cpu.time", [guard(cpu)], unit="s", description="CPU time used by the process."
    )
    meter_instance.create_observable_up_down_counter(
        "process.thread.count",
        [guard(lambda: [Observation(threading.active_count())])],
        unit="{thread}",
        description="Threads alive in the process.",
    )
    meter_instance.create_observable_counter(
        "cpython.gc.collections",
        [guard(collections)],
        unit="{collection}",
        description="Garbage collections per generation.",
    )
    meter_instance.create_observable_gauge(
        "process.uptime",
        [guard(lambda: [Observation(time.monotonic() - _IMPORTED_AT)])],
        unit="s",
        description="Time since the SDK was loaded (approximately the process uptime).",
    )
    return handle


_IMPORTED_AT = time.monotonic()
