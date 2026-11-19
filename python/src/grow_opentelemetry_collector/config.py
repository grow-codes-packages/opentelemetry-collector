"""Configuration of the OpenTelemetry Collector client and of every OTLP exporter this package
builds (traces, logs, metrics).

This file is owned by the service package (generated once by sdk-kit).
"""

from __future__ import annotations

import dataclasses
import os
import sys
from collections.abc import Mapping, Sequence
from types import MappingProxyType
from typing import Any, ClassVar, Literal, TypeVar
from urllib.parse import unquote, urlsplit

from ._core import BaseConfig, ConfigError, EnvReader, RetryPolicy, TlsConfig, parse_duration
from ._core.env import to_seconds
from ._core.redaction import is_sensitive_header

__all__ = [
    "ALIAS_ENV_PREFIX",
    "DEFAULT_EXPORT_TIMEOUT_S",
    "DEFAULT_GRPC_URL",
    "DEFAULT_METRIC_EXPORT_INTERVAL_S",
    "DEFAULT_URL",
    "RESOURCE_DETECTORS",
    "SIGNALS",
    "VENDOR_ENV",
    "OtelCollectorConfig",
    "OtlpProtocol",
    "ResourceDetectorName",
    "Signal",
    "parse_key_value_list",
]

#: Default OTLP/HTTP base URL (signal paths ``/v1/traces`` … are appended).
DEFAULT_URL = "http://127.0.0.1:4318"
#: Default OTLP/gRPC target.
DEFAULT_GRPC_URL = "http://127.0.0.1:4317"
#: Default deadline of one OTLP export, in seconds.
DEFAULT_EXPORT_TIMEOUT_S = 10.0
#: Default interval of the periodic metric reader, in seconds.
DEFAULT_METRIC_EXPORT_INTERVAL_S = 60.0
#: Alias prefix accepted below ``GROW_OPENTELEMETRY_COLLECTOR_`` (ticket wording).
ALIAS_ENV_PREFIX = "GROW_OTEL_"

#: OTLP transport: ``http/protobuf`` (port 4318, default) or ``grpc`` (port 4317).
OtlpProtocol = Literal["http/protobuf", "grpc"]
#: One of the three OpenTelemetry signals.
Signal = Literal["traces", "logs", "metrics"]
#: A resource detector run by ``init_telemetry``.
ResourceDetectorName = Literal["host", "process", "container", "k8s"]

#: Every signal, in a fixed order.
SIGNALS: tuple[Signal, ...] = ("traces", "logs", "metrics")
#: Every resource detector, in a fixed order.
RESOURCE_DETECTORS: tuple[ResourceDetectorName, ...] = ("host", "process", "container", "k8s")

#: Standard OpenTelemetry variables consulted after ``GROW_OPENTELEMETRY_COLLECTOR_*`` and
#: ``GROW_OTEL_*``.
VENDOR_ENV: Mapping[str, str] = MappingProxyType(
    {
        "endpoint": "OTEL_EXPORTER_OTLP_ENDPOINT",
        "protocol": "OTEL_EXPORTER_OTLP_PROTOCOL",
        "headers": "OTEL_EXPORTER_OTLP_HEADERS",
        "compression": "OTEL_EXPORTER_OTLP_COMPRESSION",
        "timeout": "OTEL_EXPORTER_OTLP_TIMEOUT",
        "certificate": "OTEL_EXPORTER_OTLP_CERTIFICATE",
        "client_certificate": "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
        "client_key": "OTEL_EXPORTER_OTLP_CLIENT_KEY",
        "service_name": "OTEL_SERVICE_NAME",
        "resource_attributes": "OTEL_RESOURCE_ATTRIBUTES",
        "sdk_disabled": "OTEL_SDK_DISABLED",
        "sampler": "OTEL_TRACES_SAMPLER",
        "sampler_arg": "OTEL_TRACES_SAMPLER_ARG",
        "propagators": "OTEL_PROPAGATORS",
        "metric_export_interval": "OTEL_METRIC_EXPORT_INTERVAL",
    }
)

_PROTOCOLS = ("http/protobuf", "grpc")
_SAMPLERS = (
    "always_on",
    "always_off",
    "traceidratio",
    "parentbased_always_on",
    "parentbased_always_off",
    "parentbased_traceidratio",
)
_TRACES_BATCH_KEYS = (
    "max_queue_size",
    "max_export_batch_size",
    "scheduled_delay_s",
    "export_timeout_s",
)
_LOGS_BATCH_KEYS = (
    "max_queue_size",
    "max_batch_size",
    "max_batch_bytes",
    "flush_interval_s",
    "export_timeout_s",
    "drop_policy",
    "gzip",
    "max_export_retries",
    "retry_backoff_s",
)
_MAP_FIELDS = (
    "headers",
    "traces_headers",
    "logs_headers",
    "metrics_headers",
    "resource_attributes",
    "traces_batch",
    "logs_batch",
)

_C = TypeVar("_C", bound="OtelCollectorConfig")


def parse_key_value_list(raw: str, source: str) -> dict[str, str]:
    """Parse ``k=v,k2=v2`` (values percent-decoded) as used by ``OTEL_EXPORTER_OTLP_HEADERS``
    and ``OTEL_RESOURCE_ATTRIBUTES``.

    Raises :class:`ConfigError` naming ``source`` (never the value) on malformed input.
    """
    out: dict[str, str] = {}
    for part in raw.split(","):
        if not part.strip():
            continue
        key, sep, value = part.partition("=")
        if not sep or not key.strip():
            raise ConfigError(
                f"config: invalid value in environment variable {source}: expected key=value pairs"
            )
        out[unquote(key.strip())] = unquote(value.strip())
    return out


def _default_service_name() -> str:
    name = os.path.basename(sys.executable or "") or "python"
    return f"unknown_service:{name}"


def _is_http_url(value: str) -> bool:
    try:
        parts = urlsplit(value)
    except ValueError:
        return False
    return parts.scheme in ("http", "https") and bool(parts.hostname)


def _sibling(base: str, port: int, path: str) -> str:
    """URL with the scheme and host of ``base`` and another port and path."""
    parts = urlsplit(base)
    host = parts.hostname or "127.0.0.1"
    if ":" in host:
        host = f"[{host}]"
    return f"{parts.scheme}://{host}:{port}{path}"


def _invalid(name: str, expected: str) -> ConfigError:
    return ConfigError(f"config: invalid value in environment variable {name}: expected {expected}")


def _parse_protocol(value: str, name: str) -> str:
    v = value.strip().lower()
    if v not in _PROTOCOLS:
        raise _invalid(name, "grpc or http/protobuf")
    return v


def _parse_compression(value: str, name: str) -> str:
    v = value.strip().lower()
    if v not in ("gzip", "none"):
        raise _invalid(name, "gzip or none")
    return v


def _parse_sampler(value: str, name: str) -> str:
    v = value.strip().lower()
    if v not in _SAMPLERS:
        raise _invalid(name, "a known sampler name")
    return v


def _parse_ratio(value: str, name: str) -> float:
    try:
        ratio = float(value.strip())
    except ValueError:
        ratio = -1.0
    if not 0.0 <= ratio <= 1.0:
        raise _invalid(name, "a ratio between 0 and 1")
    return ratio


def _parse_list(value: str) -> tuple[str, ...]:
    return tuple(p.strip() for p in value.split(",") if p.strip())


def _parse_detectors(value: str, name: str) -> tuple[str, ...]:
    names = _parse_list(value.lower())
    if names == ("none",):
        return ()
    for d in names:
        if d not in RESOURCE_DETECTORS:
            raise _invalid(name, "host, process, container, k8s or none")
    return names


class _OtelEnv:
    """Reads plain ``OTEL_*`` variables (empty = unset)."""

    def __init__(self, env: Mapping[str, str]) -> None:
        self.env = env

    def get(self, name: str) -> str | None:
        raw = self.env.get(name)
        if raw is None or not raw.strip():
            return None
        return raw.strip()

    def positive_int(self, name: str) -> int | None:
        raw = self.get(name)
        if raw is None:
            return None
        if not raw.isdigit() or int(raw) <= 0:
            raise _invalid(name, "a positive integer")
        return int(raw)

    def seconds(self, name: str) -> float | None:
        raw = self.get(name)
        if raw is None:
            return None
        try:
            value = parse_duration(raw)
        except ValueError:
            value = 0.0
        if value <= 0:
            raise _invalid(name, "a duration in milliseconds")
        return value


def _tls_dict(tls: TlsConfig | None) -> dict[str, Any]:
    if tls is None:
        return {}
    return {
        f.name: getattr(tls, f.name)
        for f in dataclasses.fields(tls)
        if getattr(tls, f.name) not in (None, False)
    }


def _grow_layer(
    env: Mapping[str, str], prefix: str, common: type[BaseConfig] = BaseConfig
) -> dict[str, Any]:
    """One ``GROW_*`` layer (primary prefix or the ``GROW_OTEL_`` alias). ``common`` reads
    the common fields (its ``ENV_VENDOR`` variables rank just below the prefixed name)."""
    reader = EnvReader(env, prefix)
    layer: dict[str, Any] = common._env_values(reader)
    if "tls" in layer:
        layer["tls"] = _tls_dict(layer["tls"])
    if "retry" in layer:
        layer["max_retries"] = layer.pop("retry").max_retries

    def put(key: str, value: Any) -> None:
        if value is not None:
            layer[key] = value

    put("grpc_url", reader.string("GRPC_URL"))
    protocol = reader.lookup("PROTOCOL")
    if protocol is not None:
        layer["protocol"] = _parse_protocol(protocol[1], protocol[0])
    for signal in SIGNALS:
        put(f"{signal}_url", reader.string(f"{signal.upper()}_URL"))
    headers = reader.lookup("HEADERS")
    if headers is not None:
        layer["headers"] = parse_key_value_list(headers[1], headers[0])
    compression = reader.lookup("COMPRESSION")
    if compression is not None:
        layer["compression"] = _parse_compression(compression[1], compression[0])
    put("export_timeout", reader.duration("EXPORT_TIMEOUT"))
    put("health_url", reader.string("HEALTH_URL"))
    put("zpages_url", reader.string("ZPAGES_URL"))
    put("telemetry_url", reader.string("TELEMETRY_URL"))
    put("service_name", reader.string("SERVICE_NAME"))
    put("service_version", reader.string("SERVICE_VERSION"))
    put("environment", reader.string("ENVIRONMENT"))
    attrs = reader.lookup("RESOURCE_ATTRIBUTES")
    if attrs is not None:
        layer["resource_attributes"] = parse_key_value_list(attrs[1], attrs[0])
    for signal in SIGNALS:
        put(signal, reader.bool(signal.upper()))
    sampler = reader.lookup("SAMPLER")
    if sampler is not None:
        layer["sampler"] = _parse_sampler(sampler[1], sampler[0])
    ratio = reader.lookup("SAMPLER_RATIO")
    if ratio is not None:
        layer["sampler_ratio"] = _parse_ratio(ratio[1], ratio[0])
    propagators = reader.string("PROPAGATORS")
    if propagators is not None:
        layer["propagators"] = _parse_list(propagators)
    put("metric_export_interval", reader.duration("METRIC_EXPORT_INTERVAL"))
    put("runtime_metrics", reader.bool("RUNTIME_METRICS"))
    detectors = reader.lookup("RESOURCE_DETECTORS")
    if detectors is not None:
        layer["resource_detectors"] = _parse_detectors(detectors[1], detectors[0])
    return layer


def _otel_layer(env: Mapping[str, str], higher_protocol: str | None) -> dict[str, Any]:
    """The standard ``OTEL_*`` layer. ``higher_protocol`` (decided by the upper layers first)
    picks whether ``OTEL_EXPORTER_OTLP_ENDPOINT`` feeds ``url`` or ``grpc_url``."""
    otel = _OtelEnv(env)
    layer: dict[str, Any] = {}
    raw_protocol = otel.get(VENDOR_ENV["protocol"])
    if raw_protocol is not None:
        layer["protocol"] = _parse_protocol(raw_protocol, VENDOR_ENV["protocol"])
    protocol = higher_protocol or layer.get("protocol") or "http/protobuf"
    endpoint = otel.get(VENDOR_ENV["endpoint"])
    if endpoint is not None:
        layer["grpc_url" if protocol == "grpc" else "url"] = endpoint
    for signal in SIGNALS:
        upper = signal.upper()
        signal_endpoint = otel.get(f"OTEL_EXPORTER_OTLP_{upper}_ENDPOINT")
        if signal_endpoint is not None:
            layer[f"{signal}_url"] = signal_endpoint
        signal_headers = otel.get(f"OTEL_EXPORTER_OTLP_{upper}_HEADERS")
        if signal_headers is not None:
            layer[f"{signal}_headers"] = parse_key_value_list(
                signal_headers, f"OTEL_EXPORTER_OTLP_{upper}_HEADERS"
            )
    headers = otel.get(VENDOR_ENV["headers"])
    if headers is not None:
        layer["headers"] = parse_key_value_list(headers, VENDOR_ENV["headers"])
    compression = otel.get(VENDOR_ENV["compression"])
    if compression is not None:
        layer["compression"] = _parse_compression(compression, VENDOR_ENV["compression"])
    timeout = otel.seconds(VENDOR_ENV["timeout"])
    if timeout is not None:
        layer["export_timeout"] = timeout
    tls: dict[str, Any] = {}
    for field, key in (
        ("ca_cert_path", "certificate"),
        ("client_cert_path", "client_certificate"),
        ("client_key_path", "client_key"),
    ):
        path = otel.get(VENDOR_ENV[key])
        if path is not None:
            tls[field] = path
    if tls:
        layer["tls"] = tls
    raw_resource = otel.get(VENDOR_ENV["resource_attributes"])
    resource = (
        parse_key_value_list(raw_resource, VENDOR_ENV["resource_attributes"])
        if raw_resource is not None
        else {}
    )
    resource_service = resource.pop("service.name", None)
    if raw_resource is not None:
        layer["resource_attributes"] = resource
    service_name = otel.get(VENDOR_ENV["service_name"]) or resource_service
    if service_name:
        layer["service_name"] = service_name
    disabled = otel.get(VENDOR_ENV["sdk_disabled"])
    if disabled is not None and disabled.lower() == "true":
        for signal in SIGNALS:
            layer[signal] = False
    else:
        for signal in SIGNALS:
            exporter = otel.get(f"OTEL_{signal.upper()}_EXPORTER")
            if exporter is not None and exporter.lower() == "none":
                layer[signal] = False
    sampler = otel.get(VENDOR_ENV["sampler"])
    if sampler is not None:
        layer["sampler"] = _parse_sampler(sampler, VENDOR_ENV["sampler"])
        arg = otel.get(VENDOR_ENV["sampler_arg"])
        if arg is not None and layer["sampler"].endswith("traceidratio"):
            layer["sampler_ratio"] = _parse_ratio(arg, VENDOR_ENV["sampler_arg"])
    propagators = otel.get(VENDOR_ENV["propagators"])
    if propagators is not None:
        layer["propagators"] = _parse_list(propagators)
    interval = otel.seconds(VENDOR_ENV["metric_export_interval"])
    if interval is not None:
        layer["metric_export_interval"] = interval

    bsp: dict[str, Any] = {}
    for key, name, kind in (
        ("scheduled_delay_s", "OTEL_BSP_SCHEDULE_DELAY", "s"),
        ("export_timeout_s", "OTEL_BSP_EXPORT_TIMEOUT", "s"),
        ("max_queue_size", "OTEL_BSP_MAX_QUEUE_SIZE", "n"),
        ("max_export_batch_size", "OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "n"),
    ):
        value = otel.seconds(name) if kind == "s" else otel.positive_int(name)
        if value is not None:
            bsp[key] = value
    if bsp:
        layer["traces_batch"] = bsp
    blrp: dict[str, Any] = {}
    for key, name, kind in (
        ("flush_interval_s", "OTEL_BLRP_SCHEDULE_DELAY", "s"),
        ("export_timeout_s", "OTEL_BLRP_EXPORT_TIMEOUT", "s"),
        ("max_queue_size", "OTEL_BLRP_MAX_QUEUE_SIZE", "n"),
        ("max_batch_size", "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE", "n"),
    ):
        value = otel.seconds(name) if kind == "s" else otel.positive_int(name)
        if value is not None:
            blrp[key] = value
    if blrp:
        layer["logs_batch"] = blrp
    return layer


def _merge(*layers: Mapping[str, Any]) -> dict[str, Any]:
    """Later layers win; map fields and TLS merge key by key."""
    merged: dict[str, Any] = {}
    for layer in layers:
        for key, value in layer.items():
            if value is None:
                continue
            if key in _MAP_FIELDS or key == "tls":
                previous = merged.get(key) or {}
                current = _tls_dict(value) if isinstance(value, TlsConfig) else dict(value)
                merged[key] = {**dict(previous), **current}
            else:
                merged[key] = value
    return merged


def _frozen(value: Mapping[str, Any] | None) -> Mapping[str, Any]:
    return MappingProxyType(dict(value or {}))


@dataclasses.dataclass(frozen=True, repr=False)
class OtelCollectorConfig(BaseConfig):
    """Immutable settings of :class:`~grow_opentelemetry_collector.OtelCollectorClient` and of
    the OTLP exporters (``init_telemetry``, ``init_tracing``, log client, metrics).

    Common fields (``url``, timeouts, ``retry``, ``tls``, ``proxy``, ``headers``,
    ``user_agent_suffix``, ``hooks``, ``logger``) come from :class:`BaseConfig`; ``url`` is the
    OTLP/HTTP base URL (default ``http://127.0.0.1:4318``). Durations are float seconds
    (``timedelta`` accepted). The collector has no authentication of its own: credentials of a
    gateway or authenticator go in ``headers`` (redacted everywhere).

    Fields left as ``None`` are derived: ``grpc_url`` = host of ``url`` on port 4317,
    ``health_url`` = port 13133 path ``/``, ``zpages_url`` = port 55679, ``service_name`` =
    ``unknown_service:<executable>``. Map fields (``traces_batch``: keys of
    :class:`SpanBatchOptions`; ``logs_batch``: keys of :class:`BatchOptions`) hold only the
    settings that differ from the defaults.

    See :meth:`from_env` for the environment variables.
    """

    ENV_VENDOR: ClassVar[Mapping[str, tuple[str, ...]]] = {}

    url: str = DEFAULT_URL
    #: OTLP/gRPC target (``https://`` enables TLS). Default: host of ``url``, port 4317.
    grpc_url: str | None = None
    #: Export protocol. ``native`` is rejected by :meth:`validate`.
    protocol: str = "http/protobuf"
    #: Full OTLP/HTTP traces URL, used as-is (overrides ``url`` + ``/v1/traces``).
    traces_url: str | None = None
    #: Full OTLP/HTTP logs URL, used as-is.
    logs_url: str | None = None
    #: Full OTLP/HTTP metrics URL, used as-is.
    metrics_url: str | None = None
    #: Extra headers of trace exports (merged over ``headers``). Redacted.
    traces_headers: Mapping[str, str] = dataclasses.field(default_factory=dict)
    #: Extra headers of log exports (merged over ``headers``). Redacted.
    logs_headers: Mapping[str, str] = dataclasses.field(default_factory=dict)
    #: Extra headers of metric exports (merged over ``headers``). Redacted.
    metrics_headers: Mapping[str, str] = dataclasses.field(default_factory=dict)
    #: Payload compression of OTLP exports: ``gzip`` (default) or ``none``.
    compression: str = "gzip"
    #: Deadline of one OTLP export including the exporter's own retries, in seconds.
    export_timeout: float = DEFAULT_EXPORT_TIMEOUT_S
    #: ``health_check`` extension URL. Default: host of ``url``, port 13133, path ``/``.
    health_url: str | None = None
    #: zpages extension base URL. Default: host of ``url``, port 55679.
    zpages_url: str | None = None
    #: Internal telemetry (Prometheus) URL such as ``http://collector:8888/metrics``.
    telemetry_url: str | None = None
    #: ``service.name``. Default ``unknown_service:<executable>``.
    service_name: str | None = None
    #: ``service.version``.
    service_version: str | None = None
    #: ``deployment.environment.name``.
    environment: str | None = None
    #: Resource attributes; explicit keys win over environment keys.
    resource_attributes: Mapping[str, Any] = dataclasses.field(default_factory=dict)
    #: Export traces (default true).
    traces: bool = True
    #: Export logs (default true).
    logs: bool = True
    #: Export metrics (default true).
    metrics: bool = True
    #: Trace sampler (default ``parentbased_always_on``).
    sampler: str = "parentbased_always_on"
    #: Ratio of the ``*traceidratio`` samplers (0..1).
    sampler_ratio: float = 1.0
    #: Propagators (default ``tracecontext`` and ``baggage``).
    propagators: Sequence[str] = ("tracecontext", "baggage")
    #: Batch span processor settings that differ from the defaults.
    traces_batch: Mapping[str, Any] = dataclasses.field(default_factory=dict)
    #: Log batch settings that differ from the defaults.
    logs_batch: Mapping[str, Any] = dataclasses.field(default_factory=dict)
    #: Interval of the periodic metric reader, in seconds (default 60).
    metric_export_interval: float = DEFAULT_METRIC_EXPORT_INTERVAL_S
    #: Report Python runtime metrics (memory, CPU, threads, GC, uptime).
    runtime_metrics: bool = False
    #: Resource detectors run by ``init_telemetry`` (default all).
    resource_detectors: Sequence[str] = RESOURCE_DETECTORS

    def __post_init__(self) -> None:
        super().__post_init__()
        for name in ("export_timeout", "metric_export_interval"):
            object.__setattr__(self, name, to_seconds(getattr(self, name)))
        for name in _MAP_FIELDS[1:]:
            object.__setattr__(self, name, _frozen(getattr(self, name)))
        object.__setattr__(self, "propagators", tuple(self.propagators))
        object.__setattr__(self, "resource_detectors", tuple(self.resource_detectors))
        url_ok = _is_http_url(self.url)
        if self.grpc_url is None:
            grpc = _sibling(self.url, 4317, "") if url_ok else DEFAULT_GRPC_URL
            object.__setattr__(self, "grpc_url", grpc)
        if self.health_url is None and url_ok:
            object.__setattr__(self, "health_url", _sibling(self.url, 13133, "/"))
        if self.zpages_url is None and url_ok:
            object.__setattr__(self, "zpages_url", _sibling(self.url, 55679, ""))
        name = (self.service_name or "").strip()
        object.__setattr__(self, "service_name", name or _default_service_name())

    # ------------------------------------------------------------------ environment

    @classmethod
    def from_env(cls: type[_C], env: Mapping[str, str] | None = None, **overrides: Any) -> _C:
        """Build a validated config from the environment.

        Precedence per field: explicit keyword > ``GROW_OPENTELEMETRY_COLLECTOR_<NAME>`` >
        ``GROW_OTEL_<NAME>`` > per-signal ``OTEL_EXPORTER_OTLP_<SIGNAL>_*`` > generic
        ``OTEL_*`` > default. Map fields (``headers``, ``resource_attributes``, per-signal
        headers, batch settings) and ``tls`` merge key by key in the same order.

        ``GROW_*`` names (after the prefix): ``URL``, ``GRPC_URL``, ``PROTOCOL``,
        ``TRACES_URL``, ``LOGS_URL``, ``METRICS_URL``, ``HEADERS`` (``k=v,…``),
        ``COMPRESSION``, ``EXPORT_TIMEOUT``, ``HEALTH_URL``, ``ZPAGES_URL``,
        ``TELEMETRY_URL``, ``SERVICE_NAME``, ``SERVICE_VERSION``, ``ENVIRONMENT``,
        ``RESOURCE_ATTRIBUTES``, ``TRACES``/``LOGS``/``METRICS`` (bool), ``SAMPLER``,
        ``SAMPLER_RATIO``, ``PROPAGATORS``, ``METRIC_EXPORT_INTERVAL``, ``RUNTIME_METRICS``,
        ``RESOURCE_DETECTORS`` (list or ``none``) and the common ``CONNECT_TIMEOUT``,
        ``REQUEST_TIMEOUT``, ``TOTAL_TIMEOUT``, ``MAX_RETRIES``, ``CA_CERT``, ``CLIENT_CERT``,
        ``CLIENT_KEY``, ``INSECURE_SKIP_VERIFY``, ``PROXY``.

        ``OTEL_*``: ``OTEL_EXPORTER_OTLP_ENDPOINT`` (→ ``url``, or ``grpc_url`` for gRPC),
        ``…_PROTOCOL``, ``…_HEADERS``, ``…_COMPRESSION``, ``…_TIMEOUT`` (ms),
        ``…_CERTIFICATE``, ``…_CLIENT_CERTIFICATE``, ``…_CLIENT_KEY``,
        ``OTEL_EXPORTER_OTLP_{TRACES,LOGS,METRICS}_{ENDPOINT,HEADERS}`` (endpoints used as-is),
        ``OTEL_SERVICE_NAME`` (wins over ``service.name`` in ``OTEL_RESOURCE_ATTRIBUTES``),
        ``OTEL_RESOURCE_ATTRIBUTES``, ``OTEL_SDK_DISABLED``, ``OTEL_{SIGNAL}_EXPORTER=none``,
        ``OTEL_TRACES_SAMPLER``/``_ARG``, ``OTEL_PROPAGATORS``, ``OTEL_METRIC_EXPORT_INTERVAL``,
        ``OTEL_BSP_*`` and ``OTEL_BLRP_*``.

        ``env`` defaults to ``os.environ``. A present but invalid value raises
        :class:`ConfigError` naming the variable, never its value.
        """
        source: Mapping[str, str] = os.environ if env is None else env
        primary = _grow_layer(source, cls.ENV_PREFIX, cls)
        alias = _grow_layer(source, ALIAS_ENV_PREFIX)
        explicit = {k: v for k, v in overrides.items() if v is not None}
        higher = explicit.get("protocol") or primary.get("protocol") or alias.get("protocol")
        otel = _otel_layer(source, higher)
        merged = _merge(otel, alias, primary, explicit)
        if "tls" in merged:
            merged["tls"] = TlsConfig(**merged["tls"])
        retries = merged.pop("max_retries", None)
        if retries is not None and "retry" not in explicit:
            merged["retry"] = RetryPolicy().with_max_retries(retries)
        return cls(**merged).validate()

    #: Prefix of every primary environment variable.
    ENV_PREFIX: ClassVar[str] = "GROW_OPENTELEMETRY_COLLECTOR_"

    # ------------------------------------------------------------------ derived values

    def signal_url(self, signal: Signal) -> str:
        """OTLP/HTTP URL of a signal: the explicit ``<signal>_url``, else ``url`` +
        ``/v1/<signal>``."""
        explicit: str | None = getattr(self, f"{signal}_url")
        if explicit:
            return explicit
        return f"{self.url.rstrip('/')}/v1/{signal}"

    def endpoint_for(self, signal: Signal) -> str:
        """Export endpoint of a signal for the configured protocol (``grpc_url`` for gRPC)."""
        if self.protocol == "grpc":
            return str(self.grpc_url)
        return self.signal_url(signal)

    def headers_for(self, signal: Signal) -> dict[str, str]:
        """Export headers of a signal: ``headers`` with the per-signal headers on top."""
        return {**dict(self.headers), **dict(getattr(self, f"{signal}_headers"))}

    def known_secrets(self) -> list[str]:
        """Values of sensitive export headers (``authorization``, ``*token*``, ``*key*`` …)."""
        out = super().known_secrets()
        for mapping in (self.headers, self.traces_headers, self.logs_headers, self.metrics_headers):
            for key, value in mapping.items():
                if not value or not is_sensitive_header(key):
                    continue
                out.append(value)
                scheme, _, credential = value.partition(" ")
                if credential.strip() and scheme.isalpha():
                    out.append(credential.strip())  # "Bearer <token>" → also "<token>"
        return out

    # ------------------------------------------------------------------ validation

    def validate(self: _C) -> _C:
        """Common checks plus the collector rules; raises :class:`ConfigError`."""
        super().validate()
        if self.protocol == "native":
            raise ConfigError(
                "config: the OpenTelemetry Collector has no native protocol; "
                "use grpc or http/protobuf"
            )
        problems: list[str] = []
        if self.protocol not in _PROTOCOLS:
            problems.append("protocol must be grpc or http/protobuf")
        for name in (
            "grpc_url",
            "health_url",
            "zpages_url",
            "traces_url",
            "logs_url",
            "metrics_url",
            "telemetry_url",
        ):
            value = getattr(self, name)
            if value is not None and not _is_http_url(value):
                problems.append(f"{name} must be an http or https URL")
        if self.compression not in ("gzip", "none"):
            problems.append("compression must be gzip or none")
        for name in ("export_timeout", "metric_export_interval"):
            if not getattr(self, name) > 0:
                problems.append(f"{name} must be positive")
        if self.sampler not in _SAMPLERS:
            problems.append("sampler is unknown")
        if not 0.0 <= self.sampler_ratio <= 1.0:
            problems.append("sampler_ratio must be between 0 and 1")
        for detector in self.resource_detectors:
            if detector not in RESOURCE_DETECTORS:
                problems.append(f"resource detector {detector!r} is unknown")
        for key in self.traces_batch:
            if key not in _TRACES_BATCH_KEYS:
                problems.append(f"traces_batch key {key!r} is unknown")
        for key in self.logs_batch:
            if key not in _LOGS_BATCH_KEYS:
                problems.append(f"logs_batch key {key!r} is unknown")
        if problems:
            raise ConfigError(f"config: {'; '.join(problems)}")
        return self
