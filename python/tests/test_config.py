"""OtelCollectorConfig: defaults, environment precedence (explicit > GROW_OPENTELEMETRY_COLLECTOR_*
> GROW_OTEL_* > per-signal OTEL_* > OTEL_* > default), derived values and validation."""

from __future__ import annotations

import datetime as dt
from typing import Any

import pytest

import grow_opentelemetry_collector as sdk

P = "GROW_OPENTELEMETRY_COLLECTOR_"
A = "GROW_OTEL_"


def from_env(env: dict[str, str], **kw: Any) -> sdk.OtelCollectorConfig:
    return sdk.OtelCollectorConfig.from_env(env, **kw)


def test_defaults() -> None:
    cfg = from_env({})
    assert cfg.url == "http://127.0.0.1:4318"
    assert cfg.grpc_url == "http://127.0.0.1:4317"
    assert cfg.protocol == "http/protobuf"
    assert cfg.compression == "gzip"
    assert cfg.export_timeout == 10.0
    assert cfg.metric_export_interval == 60.0
    assert cfg.health_url == "http://127.0.0.1:13133/"
    assert cfg.zpages_url == "http://127.0.0.1:55679"
    assert cfg.telemetry_url is None
    assert cfg.service_name and cfg.service_name.startswith("unknown_service:")
    assert (cfg.traces, cfg.logs, cfg.metrics, cfg.runtime_metrics) == (True, True, True, False)
    assert cfg.sampler == "parentbased_always_on" and cfg.sampler_ratio == 1.0
    assert cfg.propagators == ("tracecontext", "baggage")
    assert cfg.resource_detectors == sdk.RESOURCE_DETECTORS
    assert cfg.signal_url("logs") == "http://127.0.0.1:4318/v1/logs"
    assert cfg.endpoint_for("metrics") == "http://127.0.0.1:4318/v1/metrics"
    grpc = sdk.OtelCollectorConfig(protocol="grpc", export_timeout=dt.timedelta(seconds=3))
    assert grpc.endpoint_for("traces") == "http://127.0.0.1:4317"
    signal = sdk.OtelCollectorConfig(
        protocol="grpc", url="http://otel:4318", traces_url="http://traces-gw:4317"
    )
    assert signal.endpoint_for("traces") == "http://traces-gw:4317"
    assert signal.endpoint_for("logs") == "http://otel:4317"
    assert grpc.export_timeout == 3.0
    with pytest.raises(TypeError):
        cfg.resource_attributes["x"] = 1  # type: ignore[index]


def test_grow_layer_reads_every_field() -> None:
    env = {
        P + "URL": "http://c:4318",
        P + "GRPC_URL": "https://c:4317",
        P + "PROTOCOL": "GRPC",
        P + "TRACES_URL": "http://t:1/x",
        P + "LOGS_URL": "http://l:1/x",
        P + "METRICS_URL": "http://m:1/x",
        P + "HEADERS": "authorization=Bearer%20abc123,x-a=1",
        P + "COMPRESSION": "none",
        P + "EXPORT_TIMEOUT": "2s",
        P + "HEALTH_URL": "http://h:1/",
        P + "ZPAGES_URL": "http://z:1",
        P + "TELEMETRY_URL": "http://t:8888/metrics",
        P + "SERVICE_NAME": "svc",
        P + "SERVICE_VERSION": "1.0",
        P + "ENVIRONMENT": "prod",
        P + "RESOURCE_ATTRIBUTES": "team=core",
        P + "TRACES": "false",
        P + "LOGS": "0",
        P + "METRICS": "no",
        P + "SAMPLER": "traceidratio",
        P + "SAMPLER_RATIO": "0.25",
        P + "PROPAGATORS": "tracecontext, b3",
        P + "METRIC_EXPORT_INTERVAL": "500",
        P + "RUNTIME_METRICS": "true",
        P + "RESOURCE_DETECTORS": "host,k8s",
        P + "MAX_RETRIES": "1",
        P + "CA_CERT": "/ca.pem",
    }
    cfg = from_env(env)
    assert (cfg.url, cfg.grpc_url, cfg.protocol) == ("http://c:4318", "https://c:4317", "grpc")
    assert (cfg.traces_url, cfg.logs_url, cfg.metrics_url) == (
        "http://t:1/x",
        "http://l:1/x",
        "http://m:1/x",
    )
    assert cfg.headers == {"authorization": "Bearer abc123", "x-a": "1"}
    assert cfg.compression == "none" and cfg.export_timeout == 2.0
    assert (cfg.health_url, cfg.zpages_url, cfg.telemetry_url) == (
        "http://h:1/",
        "http://z:1",
        "http://t:8888/metrics",
    )
    assert (cfg.service_name, cfg.service_version, cfg.environment) == ("svc", "1.0", "prod")
    assert cfg.resource_attributes == {"team": "core"}
    assert (cfg.traces, cfg.logs, cfg.metrics) == (False, False, False)
    assert (cfg.sampler, cfg.sampler_ratio) == ("traceidratio", 0.25)
    assert cfg.propagators == ("tracecontext", "b3")
    assert cfg.metric_export_interval == 0.5 and cfg.runtime_metrics
    assert cfg.resource_detectors == ("host", "k8s")
    assert cfg.retry.max_retries == 1 and cfg.tls.ca_cert_path == "/ca.pem"
    assert cfg.signal_url("traces") == "http://t:1/x"
    assert "abc123" not in repr(cfg)
    assert from_env({P + "RESOURCE_DETECTORS": "none"}).resource_detectors == ()


def test_otel_layer() -> None:
    env = {
        "OTEL_EXPORTER_OTLP_ENDPOINT": "http://otel:4318",
        "OTEL_EXPORTER_OTLP_PROTOCOL": "http/protobuf",
        "OTEL_EXPORTER_OTLP_HEADERS": "api-key=k1,x=1",
        "OTEL_EXPORTER_OTLP_TRACES_HEADERS": "x-t=2",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "http://logs:1/v1/logs",
        "OTEL_EXPORTER_OTLP_COMPRESSION": "gzip",
        "OTEL_EXPORTER_OTLP_TIMEOUT": "2500",
        "OTEL_EXPORTER_OTLP_CERTIFICATE": "/ca",
        "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE": "/c",
        "OTEL_EXPORTER_OTLP_CLIENT_KEY": "/k",
        "OTEL_RESOURCE_ATTRIBUTES": "service.name=from-attrs,team=a%20b",
        "OTEL_TRACES_SAMPLER": "parentbased_traceidratio",
        "OTEL_TRACES_SAMPLER_ARG": "0.5",
        "OTEL_PROPAGATORS": "tracecontext",
        "OTEL_METRIC_EXPORT_INTERVAL": "1000",
        "OTEL_LOGS_EXPORTER": "none",
        "OTEL_BSP_SCHEDULE_DELAY": "200",
        "OTEL_BSP_EXPORT_TIMEOUT": "3000",
        "OTEL_BSP_MAX_QUEUE_SIZE": "100",
        "OTEL_BSP_MAX_EXPORT_BATCH_SIZE": "10",
        "OTEL_BLRP_SCHEDULE_DELAY": "100",
        "OTEL_BLRP_EXPORT_TIMEOUT": "4000",
        "OTEL_BLRP_MAX_QUEUE_SIZE": "50",
        "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE": "5",
    }
    cfg = from_env(env)
    assert cfg.url == "http://otel:4318"
    assert cfg.headers == {"api-key": "k1", "x": "1"}
    assert cfg.headers_for("traces") == {"api-key": "k1", "x": "1", "x-t": "2"}
    assert cfg.logs_url == "http://logs:1/v1/logs"
    assert cfg.export_timeout == 2.5
    assert cfg.tls == sdk.TlsConfig(ca_cert_path="/ca", client_cert_path="/c", client_key_path="/k")
    assert cfg.service_name == "from-attrs"
    assert cfg.resource_attributes == {"team": "a b"}
    assert (cfg.sampler, cfg.sampler_ratio) == ("parentbased_traceidratio", 0.5)
    assert cfg.propagators == ("tracecontext",)
    assert cfg.metric_export_interval == 1.0
    assert (cfg.traces, cfg.logs, cfg.metrics) == (True, False, True)
    assert cfg.traces_batch == {
        "scheduled_delay_s": 0.2,
        "export_timeout_s": 3.0,
        "max_queue_size": 100,
        "max_export_batch_size": 10,
    }
    assert cfg.logs_batch == {
        "flush_interval_s": 0.1,
        "export_timeout_s": 4.0,
        "max_queue_size": 50,
        "max_batch_size": 5,
    }
    assert "k1" in cfg.known_secrets()
    # OTEL_SERVICE_NAME wins over service.name in the resource attributes
    named = from_env({**env, "OTEL_SERVICE_NAME": "named"})
    assert named.service_name == "named"
    # OTEL_SDK_DISABLED turns every signal off
    off = from_env({"OTEL_SDK_DISABLED": "TRUE", "OTEL_TRACES_EXPORTER": "otlp"})
    assert (off.traces, off.logs, off.metrics) == (False, False, False)
    # the sampler arg is ignored for samplers without a ratio
    on = from_env({"OTEL_TRACES_SAMPLER": "always_on", "OTEL_TRACES_SAMPLER_ARG": "0.1"})
    assert on.sampler_ratio == 1.0


def test_otel_endpoint_feeds_grpc_url_for_grpc() -> None:
    env = {"OTEL_EXPORTER_OTLP_ENDPOINT": "http://otel:4317", "OTEL_EXPORTER_OTLP_PROTOCOL": "grpc"}
    cfg = from_env(env)
    assert (cfg.protocol, cfg.grpc_url, cfg.url) == ("grpc", "http://otel:4317", sdk.DEFAULT_URL)
    # a higher layer decides the protocol before the endpoint is assigned
    http = from_env({**env, P + "PROTOCOL": "http/protobuf"})
    assert (http.protocol, http.url) == ("http/protobuf", "http://otel:4317")
    explicit = from_env(env, protocol="http/protobuf")
    assert explicit.url == "http://otel:4317"


def test_precedence_chain() -> None:
    env = {
        "OTEL_SERVICE_NAME": "otel",
        A + "SERVICE_NAME": "alias",
        P + "SERVICE_NAME": "primary",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT": "http://signal:1/v1/traces",
        A + "TRACES_URL": "http://alias:1/v1/traces",
        "OTEL_RESOURCE_ATTRIBUTES": "a=otel,b=otel,c=otel",
        A + "RESOURCE_ATTRIBUTES": "b=alias,c=alias",
        P + "RESOURCE_ATTRIBUTES": "c=primary",
        "OTEL_EXPORTER_OTLP_HEADERS": "h1=otel",
        A + "HEADERS": "h2=alias",
    }
    cfg = from_env(env)
    assert cfg.service_name == "primary"
    assert cfg.traces_url == "http://alias:1/v1/traces"
    assert cfg.resource_attributes == {"a": "otel", "b": "alias", "c": "primary"}
    assert cfg.headers == {"h1": "otel", "h2": "alias"}
    del env[P + "SERVICE_NAME"]
    assert from_env(env).service_name == "alias"
    explicit = from_env(env, service_name="explicit", resource_attributes={"d": 1})
    assert explicit.service_name == "explicit"
    assert explicit.resource_attributes == {"a": "otel", "b": "alias", "c": "primary", "d": 1}
    tls = from_env(
        {
            "OTEL_EXPORTER_OTLP_CERTIFICATE": "/otel-ca",
            P + "CLIENT_CERT": "/c",
            P + "CLIENT_KEY": "/k",
        }
    )
    assert tls.tls == sdk.TlsConfig(
        ca_cert_path="/otel-ca", client_cert_path="/c", client_key_path="/k"
    )
    retry = from_env({P + "MAX_RETRIES": "7"}, retry=sdk.RetryPolicy(max_retries=1))
    assert retry.retry.max_retries == 1


@pytest.mark.parametrize(
    "name",
    [
        P + "PROTOCOL",
        P + "COMPRESSION",
        P + "SAMPLER",
        P + "SAMPLER_RATIO",
        P + "RESOURCE_DETECTORS",
        P + "HEADERS",
        P + "TRACES",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
        "OTEL_EXPORTER_OTLP_COMPRESSION",
        "OTEL_EXPORTER_OTLP_TIMEOUT",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
        "OTEL_RESOURCE_ATTRIBUTES",
        "OTEL_TRACES_SAMPLER",
        "OTEL_BSP_MAX_QUEUE_SIZE",
        "OTEL_METRIC_EXPORT_INTERVAL",
    ],
)
def test_invalid_env_names_variable_not_value(name: str) -> None:
    value = "bogus-value-1234"
    with pytest.raises(sdk.ConfigError) as info:
        from_env({name: value})
    assert name in str(info.value)
    assert value not in str(info.value)


def test_invalid_sampler_arg() -> None:
    with pytest.raises(sdk.ConfigError) as info:
        from_env({"OTEL_TRACES_SAMPLER": "traceidratio", "OTEL_TRACES_SAMPLER_ARG": "2"})
    assert "OTEL_TRACES_SAMPLER_ARG" in str(info.value)


def test_parse_key_value_list() -> None:
    assert sdk.parse_key_value_list("a=1, b = x%3Dy ,,", "SRC") == {"a": "1", "b": "x=y"}
    for bad in ("novalue", "=v"):
        with pytest.raises(sdk.ConfigError) as info:
            sdk.parse_key_value_list(bad, "SRC")
        assert "SRC" in str(info.value)


@pytest.mark.parametrize(
    ("kwargs", "fragment"),
    [
        ({"protocol": "native"}, "native"),
        ({"protocol": "http/json"}, "protocol"),
        ({"grpc_url": "grpc://x"}, "grpc_url"),
        ({"health_url": "nope"}, "health_url"),
        ({"telemetry_url": "ftp://x/metrics"}, "telemetry_url"),
        ({"compression": "br"}, "compression"),
        ({"export_timeout": 0}, "export_timeout"),
        ({"metric_export_interval": -1}, "metric_export_interval"),
        ({"sampler": "nope"}, "sampler"),
        ({"sampler_ratio": 2}, "sampler_ratio"),
        ({"resource_detectors": ["gpu"]}, "gpu"),
        ({"traces_batch": {"bogus": 1}}, "traces_batch"),
        ({"logs_batch": {"bogus": 1}}, "logs_batch"),
    ],
)
def test_validate(kwargs: dict[str, Any], fragment: str) -> None:
    with pytest.raises(sdk.ConfigError) as info:
        sdk.OtelCollectorConfig(**kwargs).validate()
    assert fragment in str(info.value)


def test_invalid_base_url_keeps_defaults_out() -> None:
    cfg = sdk.OtelCollectorConfig(url="not a url")
    assert cfg.grpc_url == sdk.DEFAULT_GRPC_URL
    assert cfg.health_url is None and cfg.zpages_url is None
    with pytest.raises(sdk.ConfigError):
        cfg.validate()


def test_known_secrets_from_headers() -> None:
    cfg = sdk.OtelCollectorConfig(
        headers={"Authorization": "Bearer tok-1234", "Accept": "x"},
        logs_headers={"X-Api-Key": "key-5678", "X-Empty-Token": ""},
        metrics_headers={"Authorization": "Basic"},
    )
    assert cfg.known_secrets() == ["Bearer tok-1234", "tok-1234", "key-5678", "Basic"]
