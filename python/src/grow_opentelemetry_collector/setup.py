"""Tracing preset for the OpenTelemetry Collector: the official OTLP span exporters
(``http/protobuf`` or ``grpc``) and ``init_tracing`` with collector defaults from the
environment. Service-owned module.
"""

from __future__ import annotations

import dataclasses
from collections.abc import Mapping, Sequence
from typing import Any, Literal
from urllib.parse import urlsplit

from opentelemetry.sdk.trace import SpanProcessor
from opentelemetry.sdk.trace.export import SpanExporter
from opentelemetry.sdk.trace.id_generator import IdGenerator

from ._core import ConfigError, TlsConfig
from ._tracing import (
    PropagatorFactory,
    SpanBatchOptions,
    TracingConfig,
    TracingHandle,
    init_tracing_with,
)
from .config import DEFAULT_GRPC_URL, DEFAULT_URL, OtelCollectorConfig
from .otlp import grpc_credentials, grpc_metadata, grpc_module, pem_file

__all__ = [
    "CollectorTracingHandle",
    "collector_span_exporter_factory",
    "init_tracing",
    "resolve_traces_endpoint",
    "tracing_config_from",
    "tracing_defaults",
]

_FIELD_DEFAULTS: dict[str, Any] = {
    f.name: (f.default_factory() if f.default_factory is not dataclasses.MISSING else f.default)
    for f in dataclasses.fields(TracingConfig)
    if f.name != "service_name"
}


def _has_path(url: str) -> bool:
    try:
        path = urlsplit(url).path
    except ValueError:
        return True
    return path not in ("", "/")


def resolve_traces_endpoint(protocol: str, endpoint: str | None = None) -> str:
    """Traces exporter URL: for ``http/protobuf`` an endpoint without a path (a base such as
    ``http://collector:4318``) gets ``/v1/traces``; anything else is used as-is. Without an
    endpoint: the collector default for the protocol."""
    if not endpoint:
        return DEFAULT_GRPC_URL if protocol == "grpc" else f"{DEFAULT_URL}/v1/traces"
    if protocol != "grpc" and not _has_path(endpoint):
        return endpoint.rstrip("/") + "/v1/traces"
    return endpoint


def _grpc_exporter(config: TracingConfig, endpoint: str) -> SpanExporter:
    grpc = grpc_module()
    from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import (  # noqa: PLC0415
        OTLPSpanExporter as GrpcSpanExporter,
    )

    credentials = grpc_credentials(endpoint, config.tls)
    exporter: SpanExporter = GrpcSpanExporter(
        endpoint=endpoint,
        insecure=credentials is None,
        credentials=credentials,
        headers=dict(grpc_metadata(config.headers)),
        timeout=config.batch.export_timeout_s,
        compression=grpc.Compression.Gzip
        if config.compression == "gzip"
        else grpc.Compression.NoCompression,
    )
    return exporter


def http_exporter_kwargs(tls: TlsConfig | None) -> dict[str, Any]:
    """TLS keyword arguments of the official OTLP/HTTP exporters for ``tls``."""
    import requests  # noqa: PLC0415 - dependency of the OTLP/HTTP exporters

    tls = tls or TlsConfig()
    ca_path = tls.ca_cert_path or (pem_file(tls.ca_cert_pem) if tls.ca_cert_pem else None)
    session = None
    if tls.insecure_skip_verify:
        session = requests.Session()
        session.verify = False
    return {
        "certificate_file": ca_path,
        "client_key_file": tls.client_key_path,
        "client_certificate_file": tls.client_cert_path,
        "session": session,
    }


def _http_exporter(config: TracingConfig, endpoint: str) -> SpanExporter:
    from opentelemetry.exporter.otlp.proto.http import Compression  # noqa: PLC0415
    from opentelemetry.exporter.otlp.proto.http.trace_exporter import (  # noqa: PLC0415
        OTLPSpanExporter as HttpSpanExporter,
    )

    exporter: SpanExporter = HttpSpanExporter(
        endpoint=endpoint,
        headers=dict(config.headers),
        timeout=config.batch.export_timeout_s,
        compression=Compression.Gzip if config.compression == "gzip" else Compression.NoCompression,
        **http_exporter_kwargs(config.tls),
    )
    return exporter


def collector_span_exporter_factory(config: TracingConfig) -> SpanExporter:
    """The OTLP span exporter for a tracing config (the exporter factory of
    ``init_tracing_with``): ``http/protobuf`` → ``opentelemetry-exporter-otlp-proto-http``,
    ``grpc`` → ``opentelemetry-exporter-otlp-proto-grpc`` (extra ``[grpc]``). ``native``
    raises :class:`ConfigError`. Retries follow the OTLP rules of the official exporter within
    ``batch.export_timeout_s``."""
    if config.protocol == "native":
        raise ConfigError(
            "tracing: the OpenTelemetry Collector has no native protocol; use grpc or http/protobuf"
        )
    endpoint = resolve_traces_endpoint(config.protocol, config.endpoint)
    if config.protocol == "grpc":
        return _grpc_exporter(config, endpoint)
    return _http_exporter(config, endpoint)


def tracing_defaults(collector: OtelCollectorConfig) -> dict[str, Any]:
    """:class:`TracingConfig` fields derived from a collector config (used beneath explicit
    tracing settings): service, resource, endpoint, protocol, headers, sampler, propagators,
    batch (``export_timeout`` + ``traces_batch``), TLS and compression."""
    batch: dict[str, Any] = {"export_timeout_s": collector.export_timeout}
    batch.update(collector.traces_batch)
    return {
        "service_name": collector.service_name,
        "service_version": collector.service_version,
        "environment": collector.environment,
        "resource_attributes": dict(collector.resource_attributes),
        "endpoint": collector.endpoint_for("traces"),
        "protocol": collector.protocol,
        "headers": collector.headers_for("traces"),
        "sampler": collector.sampler,
        "sampler_ratio": collector.sampler_ratio,
        "propagators": tuple(collector.propagators),
        "batch": SpanBatchOptions(**batch),
        "tls": collector.tls,
        "compression": collector.compression,
    }


def _explicit(config: TracingConfig | None, overrides: Mapping[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    if config is not None:
        if config.service_name and config.service_name.strip():
            out["service_name"] = config.service_name
        for name, default in _FIELD_DEFAULTS.items():
            value = getattr(config, name)
            if value is not None and value != default:
                out[name] = value
    out.update({k: v for k, v in overrides.items() if v is not None})
    if "service_name" in out and not str(out["service_name"]).strip():
        del out["service_name"]
    return out


def _merge_tls(base: TlsConfig | None, explicit: TlsConfig | None) -> TlsConfig | None:
    if explicit is None:
        return base
    if base is None:
        return explicit
    values = dataclasses.asdict(base)
    values.update({k: v for k, v in dataclasses.asdict(explicit).items() if v})
    return TlsConfig(**values)


def tracing_config_from(
    collector: OtelCollectorConfig,
    config: TracingConfig | None = None,
    **overrides: Any,
) -> TracingConfig:
    """The effective :class:`TracingConfig`: explicit settings (fields of ``config`` that
    differ from the defaults, plus ``overrides``) over :func:`tracing_defaults`.

    ``resource_attributes`` and ``headers`` merge key by key (explicit wins); an explicit
    ``endpoint`` is resolved with :func:`resolve_traces_endpoint`; an explicit ``protocol``
    without an endpoint switches to the collector's ``grpc_url`` / traces URL.
    """
    defaults = tracing_defaults(collector)
    explicit = _explicit(config, overrides)
    merged = {**defaults, **explicit}
    merged["resource_attributes"] = {
        **defaults["resource_attributes"],
        **dict(explicit.get("resource_attributes") or {}),
    }
    merged["headers"] = {**defaults["headers"], **dict(explicit.get("headers") or {})}
    merged["tls"] = _merge_tls(defaults["tls"], explicit.get("tls"))
    protocol = merged["protocol"]
    if "endpoint" in explicit:
        merged["endpoint"] = resolve_traces_endpoint(protocol, explicit["endpoint"])
    elif "protocol" in explicit and protocol != collector.protocol:
        merged["endpoint"] = (
            collector.grpc_url if protocol == "grpc" else collector.signal_url("traces")
        )
    return TracingConfig(**merged)


class CollectorTracingHandle(TracingHandle):
    """A running collector tracing setup: :class:`TracingHandle` plus the exporter
    ``endpoint``."""

    def __init__(self, base: TracingHandle, *, endpoint: str) -> None:
        super().__init__(
            base.config, base.tracer_provider, base.propagator, is_global=base.is_global
        )
        #: The exporter endpoint in use.
        self.endpoint = endpoint

    #: Alias so a handle can live in a client's resource registry.
    def stop(self) -> None:
        """Same as :meth:`shutdown`."""
        self.shutdown()


def init_tracing(
    config: TracingConfig | None = None,
    *,
    collector: OtelCollectorConfig | None = None,
    env: Mapping[str, str] | Literal[False] | None = None,
    propagator_factories: Mapping[str, PropagatorFactory] | None = None,
    span_processors: Sequence[SpanProcessor] = (),
    id_generator: IdGenerator | None = None,
    **overrides: Any,
) -> CollectorTracingHandle:
    """Set up OpenTelemetry tracing that exports to an OpenTelemetry Collector (SPEC §5).

    OTLP ``http/protobuf`` to ``http://127.0.0.1:4318/v1/traces`` by default, or ``grpc`` to
    ``http://127.0.0.1:4317``. Pass a :class:`TracingConfig` and/or its fields as keywords
    (``service_name="checkout"``, ``protocol="grpc"``). Unset fields come from ``collector``
    (default :meth:`OtelCollectorConfig.from_env` over ``env``; ``env=False`` ignores the
    environment), i.e. ``GROW_OPENTELEMETRY_COLLECTOR_*``, ``GROW_OTEL_*`` and the standard
    ``OTEL_*`` variables. Raises :class:`ConfigError` for invalid settings.
    """
    if collector is not None:
        cfg = collector.validate()
    elif env is False:
        cfg = OtelCollectorConfig().validate()
    else:
        cfg = OtelCollectorConfig.from_env(env)
    effective = tracing_config_from(cfg, config, **overrides)
    endpoint = resolve_traces_endpoint(effective.protocol, effective.endpoint)
    effective = dataclasses.replace(effective, endpoint=endpoint)
    base = init_tracing_with(
        effective,
        collector_span_exporter_factory,
        propagator_factories=propagator_factories,
        span_processors=span_processors,
        id_generator=id_generator,
    )
    return CollectorTracingHandle(base, endpoint=endpoint)
