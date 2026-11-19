"""init_telemetry / init_tracing / metrics helpers / resource detection with local mock receivers
(HTTP mock server, in-process gRPC server) and in-memory exporters."""

from __future__ import annotations

import os
import threading
from collections.abc import Sequence
from typing import Any

import pytest
from opentelemetry import trace as otel_trace
from opentelemetry.sdk.metrics.export import (
    MetricExporter,
    MetricExportResult,
    MetricsData,
)
from opentelemetry.sdk.trace.export import SimpleSpanProcessor
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

import grow_opentelemetry_collector as sdk
from _grpc import GrpcServer
from _support import MockServer, make_config


class MemoryMetricExporter(MetricExporter):
    """Collects every exported batch (cumulative temporality)."""

    def __init__(self, fail: bool = False) -> None:
        super().__init__()
        self.batches: list[MetricsData] = []
        self.fail = fail
        self.lock = threading.Lock()

    def export(
        self, metrics_data: MetricsData, timeout_millis: float = 10_000, **kwargs: Any
    ) -> MetricExportResult:
        with self.lock:
            self.batches.append(metrics_data)
        return MetricExportResult.SUCCESS

    def force_flush(self, timeout_millis: float = 10_000) -> bool:
        return True

    def shutdown(self, timeout_millis: float = 30_000, **kwargs: Any) -> None:
        if self.fail:
            raise RuntimeError("shutdown failed")

    def metrics(self) -> dict[str, Any]:
        out: dict[str, Any] = {}
        for batch in self.batches:
            for rm in batch.resource_metrics:
                for sm in rm.scope_metrics:
                    for m in sm.metrics:
                        out[m.name] = (m, rm.resource)
        return out


class MemoryLogExporter:
    def __init__(self) -> None:
        self.records: list[sdk.LogRecord] = []

    def export(
        self, records: Sequence[sdk.LogRecord], *, gzip: bool, timeout_s: float
    ) -> sdk.ExportResult:
        self.records.extend(records)
        return sdk.ExportResult(accepted=len(records))

    def shutdown(self) -> None:
        pass


def cfg(server: MockServer, **kw: Any) -> sdk.OtelCollectorConfig:
    kw.setdefault("service_name", "svc")
    kw.setdefault("traces_batch", {"scheduled_delay_s": 0.05})
    return make_config(server.url, **kw)


# --------------------------------------------------------------------------- init_telemetry


def test_init_telemetry_all_signals(server: MockServer) -> None:
    spans = InMemorySpanExporter()
    logs = MemoryLogExporter()
    metric_exporter = MemoryMetricExporter()
    config = cfg(
        server,
        service_version="1.2.3",
        environment="test",
        resource_attributes={"grow.run": "r1"},
    )
    with sdk.init_telemetry(
        config,
        set_global=False,
        traces=sdk.TelemetryTracesOptions(
            span_processors=[SimpleSpanProcessor(spans)], sampler="always_on"
        ),
        logs=sdk.TelemetryLogsOptions(exporter=logs, min_severity=sdk.Severity.DEBUG),
        metrics=sdk.TelemetryMetricsOptions(
            exporter=metric_exporter, runtime_metrics=True, export_interval_s=60
        ),
        detectors=["host", "process"],
    ) as handle:
        assert handle.config is config
        assert handle.resource_attributes["service.name"] == "svc"
        assert handle.resource_attributes["deployment.environment.name"] == "test"
        assert handle.resource_attributes["grow.run"] == "r1"
        assert "host.name" in handle.resource_attributes
        assert handle.resource.attributes["service.version"] == "1.2.3"
        assert handle.tracer_provider is not None and handle.meter_provider is not None
        tracer = handle.tracer("it")
        with tracer.start_as_current_span("root") as root:
            handle.logger("app").debug("inside", {"k": "v"})
            trace_id = format(root.get_span_context().trace_id, "032x")
        handle.counter("requests", unit="{request}").add(3, {"tag": "a"})
        handle.up_down_counter("inflight").add(2)
        handle.gauge("temperature").set(42.5)
        hist = handle.histogram("latency", boundaries=[10, 100], unit="ms")
        for v in (5, 50, 95):
            hist.record(v)
        assert handle.meter("custom") is not None
        handle.force_flush(10)
        assert handle.tracer().start_span("x") is not None
    assert handle.is_shut_down
    handle.shutdown()
    assert [s.name for s in spans.get_finished_spans()] == ["root"]
    assert logs.records[0].trace_id == trace_id
    assert logs.records[0].resource["service.name"] == "svc"
    assert logs.records[0].attributes["logger.name"] == "app"
    found = metric_exporter.metrics()
    for name in ("requests", "inflight", "temperature", "latency"):
        assert name in found
    point = next(iter(found["latency"][0].data.data_points))
    assert (point.count, point.sum, list(point.bucket_counts)) == (3, 150, [1, 2, 0])
    for name in (
        "process.memory.usage",
        "process.cpu.time",
        "process.thread.count",
        "cpython.gc.collections",
        "process.uptime",
    ):
        assert name in found, name
    assert found["requests"][1].attributes["grow.run"] == "r1"


def test_init_telemetry_signals_off_and_noop(server: MockServer) -> None:
    config = cfg(server, traces=False, logs=False, metrics=False)
    handle = sdk.init_telemetry(config, set_global=False, detectors=[])
    assert handle.tracing is None and handle.log_client is None and handle.meter_provider is None
    assert handle.tracer_provider is None
    assert not handle.tracer().start_span("x").is_recording()
    handle.logger("x").error("dropped")
    handle.counter("c").add(1)
    handle.force_flush(1)
    handle.shutdown()
    forced = sdk.init_telemetry(
        config, set_global=False, traces=True, logs=False, metrics=False, detectors=[]
    )
    assert forced.tracing is not None
    forced.shutdown()


def test_init_telemetry_exports_to_receiver(server: MockServer) -> None:
    """Real OTLP/HTTP exporters against the mock receiver: all three signal paths are hit."""
    env = {
        "GROW_OPENTELEMETRY_COLLECTOR_URL": server.url,
        "GROW_OPENTELEMETRY_COLLECTOR_SERVICE_NAME": "envsvc",
        "OTEL_BSP_SCHEDULE_DELAY": "50",
    }
    handle = sdk.init_telemetry(env=env, set_global=False, detectors=[])
    handle.tracer().start_span("s").end()
    handle.logger().info("hello")
    handle.counter("c").add(1)
    handle.force_flush(10)
    handle.shutdown()
    paths = {s.path for s in server.seen}
    assert {"/v1/traces", "/v1/logs", "/v1/metrics"} <= paths
    assert handle.config.service_name == "envsvc"
    no_env = sdk.init_telemetry(
        env=False, set_global=False, traces=False, logs=False, metrics=False
    )
    assert no_env.config.url == sdk.DEFAULT_URL
    no_env.shutdown()


def test_init_telemetry_global_meter_and_helpers(server: MockServer) -> None:
    exporter = MemoryMetricExporter()
    handle = sdk.init_telemetry(
        cfg(server),
        traces=False,
        logs=False,
        metrics=sdk.TelemetryMetricsOptions(exporter=exporter),
        detectors=[],
    )
    sdk.counter("global_requests").add(1)
    sdk.up_down_counter("global_inflight").add(1)
    sdk.histogram("global_hist", boundaries=[1.0]).record(1)
    sdk.gauge("global_gauge").set(2)
    assert sdk.meter("m") is not None
    handle.force_flush(5)
    found = exporter.metrics()
    for name in ("global_requests", "global_inflight", "global_hist", "global_gauge"):
        assert name in found
    handle.shutdown()
    # after shutdown the helpers fall back to the OpenTelemetry global provider
    sdk.counter("after").add(1)
    assert sdk.set_global_meter_provider(None)() is None


def test_init_telemetry_errors(server: MockServer) -> None:
    with pytest.raises(sdk.ConfigError):
        sdk.init_telemetry(
            cfg(server),
            set_global=False,
            metrics=sdk.TelemetryMetricsOptions(export_interval_s=0),
            detectors=[],
        )
    failing = sdk.init_telemetry(
        cfg(server),
        set_global=False,
        traces=False,
        logs=False,
        metrics=sdk.TelemetryMetricsOptions(exporter=MemoryMetricExporter(fail=True)),
        detectors=[],
    )
    with pytest.raises(Exception, match="shutdown failed"):
        failing.shutdown()  # every provider is shut down, then the first failure is raised


def test_force_flush_timeout(server: MockServer) -> None:
    class SlowExporter(MemoryLogExporter):
        def export(
            self, records: Sequence[sdk.LogRecord], *, gzip: bool, timeout_s: float
        ) -> sdk.ExportResult:
            threading.Event().wait(1.0)
            return super().export(records, gzip=gzip, timeout_s=timeout_s)

    handle = sdk.init_telemetry(
        cfg(server),
        set_global=False,
        traces=False,
        metrics=False,
        logs=sdk.TelemetryLogsOptions(exporter=SlowExporter(), redaction=None),
        detectors=[],
    )
    assert handle.log_client is not None and handle.log_client.redaction is None
    handle.logger().info("slow")
    with pytest.raises(sdk.TimeoutError):
        handle.force_flush(0.1)
    handle.shutdown()


def test_shutdown_raises_first_failure(server: MockServer) -> None:
    handle = sdk.init_telemetry(
        cfg(server), set_global=False, metrics=False, logs=False, detectors=[]
    )
    assert handle.tracing is not None

    def boom() -> None:
        raise RuntimeError("tracing shutdown failed")

    handle.tracing.shutdown = boom  # type: ignore[method-assign]
    with pytest.raises(RuntimeError):
        handle.shutdown()


def test_telemetry_resource_attributes() -> None:
    config = sdk.OtelCollectorConfig(
        service_name="svc",
        resource_attributes={"host.name": "override"},
        resource_detectors=["host"],
    )
    attrs = sdk.telemetry_resource_attributes(config)
    assert attrs["host.name"] == "override" and attrs["service.name"] == "svc"
    assert "service.version" not in attrs


# --------------------------------------------------------------------------- init_tracing


def test_init_tracing_http_and_grpc(server: MockServer, grpc_server: GrpcServer) -> None:
    collector = cfg(server, headers={"X-A": "1"}, resource_attributes={"r": "1"})
    handle = sdk.init_tracing(collector=collector, set_global=False)
    assert handle.endpoint == server.url + "/v1/traces"
    assert handle.config.service_name == "svc" and handle.config.headers == {"X-A": "1"}
    handle.tracer("t").start_span("http-span").end()
    handle.force_flush(10)
    handle.stop()
    assert any(s.path == "/v1/traces" for s in server.seen)
    grpc_cfg = cfg(server, protocol="grpc", grpc_url=grpc_server.url, compression="none")
    g = sdk.init_tracing(collector=grpc_cfg, set_global=False)
    assert g.endpoint == grpc_server.url
    g.tracer("t").start_span("grpc-span").end()
    g.force_flush(10)
    g.shutdown()
    assert any(s.method == "traces" and s.body for s in grpc_server.seen)


def test_tracing_config_from_merges() -> None:
    collector = sdk.OtelCollectorConfig(
        url="http://c:4318",
        service_name="svc",
        resource_attributes={"a": "1", "b": "1"},
        headers={"h": "1"},
        tls=sdk.TlsConfig(ca_cert_path="/ca"),
        traces_batch={"max_queue_size": 10, "max_export_batch_size": 5},
    )
    base = sdk.tracing_config_from(collector)
    assert base.endpoint == "http://c:4318/v1/traces"
    assert base.batch.max_queue_size == 10 and base.batch.export_timeout_s == 10.0
    explicit = sdk.TracingConfig(
        service_name="other",
        resource_attributes={"b": "2"},
        headers={"h2": "2"},
        tls=sdk.TlsConfig(client_cert_path="/c", client_key_path="/k"),
        protocol="grpc",
    )
    merged = sdk.tracing_config_from(collector, explicit)
    assert merged.service_name == "other"
    assert merged.resource_attributes == {"a": "1", "b": "2"}
    assert merged.headers == {"h": "1", "h2": "2"}
    assert merged.tls == sdk.TlsConfig(
        ca_cert_path="/ca", client_cert_path="/c", client_key_path="/k"
    )
    assert merged.endpoint == "http://c:4317"
    back = sdk.tracing_config_from(
        sdk.OtelCollectorConfig(protocol="grpc", service_name="s"), protocol="http/protobuf"
    )
    assert back.endpoint == "http://127.0.0.1:4318/v1/traces"
    ep = sdk.tracing_config_from(collector, endpoint="http://other:4318", service_name=" ")
    assert ep.endpoint == "http://other:4318/v1/traces" and ep.service_name == "svc"
    no_tls = sdk.tracing_config_from(
        sdk.OtelCollectorConfig(), tls=sdk.TlsConfig(ca_cert_path="/x")
    )
    assert no_tls.tls is not None and no_tls.tls.ca_cert_path == "/x"


def test_resolve_traces_endpoint() -> None:
    assert sdk.resolve_traces_endpoint("grpc") == sdk.DEFAULT_GRPC_URL
    assert sdk.resolve_traces_endpoint("http/protobuf") == sdk.DEFAULT_URL + "/v1/traces"
    assert sdk.resolve_traces_endpoint("http/protobuf", "http://x:1/") == "http://x:1/v1/traces"
    assert sdk.resolve_traces_endpoint("http/protobuf", "http://x:1/p") == "http://x:1/p"
    assert sdk.resolve_traces_endpoint("grpc", "http://x:1") == "http://x:1"
    assert sdk.resolve_traces_endpoint("http/protobuf", "http://[::1") == "http://[::1"


def test_exporter_factory_tls_and_native(tmp_path: Any) -> None:
    with pytest.raises(sdk.ConfigError):
        sdk.collector_span_exporter_factory(sdk.TracingConfig(service_name="s", protocol="native"))
    pem = sdk.TracingConfig(
        service_name="s", tls=sdk.TlsConfig(ca_cert_pem="-----BEGIN CERTIFICATE-----\n")
    )
    exporter = sdk.collector_span_exporter_factory(pem)
    exporter.shutdown()
    insecure = sdk.TracingConfig(service_name="s", tls=sdk.TlsConfig(insecure_skip_verify=True))
    sdk.collector_span_exporter_factory(insecure).shutdown()
    with pytest.raises(sdk.ConfigError):
        sdk.collector_span_exporter_factory(
            sdk.TracingConfig(service_name="s", protocol="grpc", tls=insecure.tls)
        )
    ca = tmp_path / "ca.pem"
    ca.write_text("not a real cert")
    secure = sdk.TracingConfig(
        service_name="s",
        protocol="grpc",
        endpoint="https://collector:4317",
        tls=sdk.TlsConfig(ca_cert_path=str(ca), ca_cert_pem="x"),
    )
    sdk.collector_span_exporter_factory(secure).shutdown()
    with pytest.raises(sdk.ConfigError) as info:
        sdk.collector_span_exporter_factory(
            sdk.TracingConfig(
                service_name="s",
                protocol="grpc",
                tls=sdk.TlsConfig(ca_cert_path="/does/not/exist.pem"),
            )
        )
    assert "/does/not" not in str(info.value)


def test_init_tracing_env_and_global(server: MockServer) -> None:
    env = {"GROW_OPENTELEMETRY_COLLECTOR_URL": server.url, "OTEL_SERVICE_NAME": "from-env"}
    handle = sdk.init_tracing(env=env, set_global=False)
    assert handle.config.service_name == "from-env"
    handle.shutdown()
    plain = sdk.init_tracing(env=False, service_name="plain", set_global=False)
    assert plain.endpoint == sdk.DEFAULT_URL + "/v1/traces"
    plain.shutdown()
    glob = sdk.init_tracing(collector=cfg(server))
    assert glob.is_global == (otel_trace.get_tracer_provider() is glob.tracer_provider)
    glob.shutdown()


# --------------------------------------------------------------------------- metrics


def test_create_metric_exporter_and_provider(server: MockServer, grpc_server: GrpcServer) -> None:
    http = sdk.create_metric_exporter(cfg(server, compression="none"))
    http.shutdown()
    grpc_cfg = cfg(server, protocol="grpc", grpc_url=grpc_server.url)
    provider = sdk.create_meter_provider(grpc_cfg, resource=_resource())
    provider.get_meter("m").create_counter("grpc_counter").add(1)
    assert provider.force_flush(10_000)
    provider.shutdown()
    assert any(s.method == "metrics" for s in grpc_server.seen)


def _resource() -> Any:
    from opentelemetry.sdk.resources import Resource

    return Resource.create({})


def test_runtime_metrics_stop() -> None:
    from opentelemetry.sdk.metrics import MeterProvider
    from opentelemetry.sdk.metrics.export import InMemoryMetricReader

    reader = InMemoryMetricReader()
    provider = MeterProvider(metric_readers=[reader])
    handle = sdk.register_runtime_metrics(provider.get_meter("rt"))
    data = reader.get_metrics_data()
    assert data is not None
    names = {m.name for rm in data.resource_metrics for sm in rm.scope_metrics for m in sm.metrics}
    assert "process.memory.usage" in names
    handle.stop()
    handle.stop()
    assert handle.stopped
    reader.get_metrics_data()
    provider.shutdown()


def test_rss_fallback(monkeypatch: pytest.MonkeyPatch) -> None:
    from grow_opentelemetry_collector import metrics as m

    def broken(*_: Any, **__: Any) -> Any:
        raise OSError("no proc")

    monkeypatch.setattr("builtins.open", broken)
    assert m._rss_bytes() > 0


# --------------------------------------------------------------------------- resource detection


def test_detect_resource_attributes() -> None:
    attrs = sdk.detect_resource_attributes(["host", "process"], env={})
    assert attrs["process.pid"] == os.getpid()
    assert attrs["host.name"] and attrs["os.type"]
    assert sdk.detect_resource_attributes([], env={}) == {}
    assert "process.pid" in sdk.detect_resource_attributes()


def test_container_id_sources() -> None:
    cid = "a" * 64
    files = {"/proc/self/cgroup": f"12:memory:/docker/{cid}\n0::/"}
    assert sdk.container_id(files.get) == cid
    mount = f"1 2 0:1 /var/lib/docker/containers/{cid}/hostname /etc/hostname rw\nother line\n"
    assert sdk.container_id({"/proc/self/cgroup": "0::/", "/proc/self/mountinfo": mount}.get) == cid
    assert sdk.container_id({"/proc/self/mountinfo": "no hostname here\n/x/hostname"}.get) is None
    assert sdk.container_id(lambda _p: None) is None
    found = sdk.detect_resource_attributes(["container"], read_file=files.get)
    assert found == {"container.id": cid}


def test_k8s_detection() -> None:
    env = {
        "KUBERNETES_SERVICE_HOST": "10.0.0.1",
        "HOSTNAME": "pod-abc",
        "K8S_POD_UID": "uid",
        "NODE_NAME": "node1",
        "K8S_CONTAINER_NAME": "app",
        "K8S_DEPLOYMENT_NAME": "dep",
        "K8S_CLUSTER_NAME": "c1",
    }
    files = {"/var/run/secrets/kubernetes.io/serviceaccount/namespace": "grow\n"}
    attrs = sdk.detect_resource_attributes(["k8s"], env=env, read_file=files.get)
    assert attrs == {
        "k8s.pod.name": "pod-abc",
        "k8s.namespace.name": "grow",
        "k8s.pod.uid": "uid",
        "k8s.node.name": "node1",
        "k8s.container.name": "app",
        "k8s.deployment.name": "dep",
        "k8s.cluster.name": "c1",
    }
    explicit = sdk.detect_resource_attributes(
        ["k8s"], env={"POD_NAME": "p", "POD_NAMESPACE": "ns", "HOSTNAME": "h"}
    )
    assert explicit == {"k8s.pod.name": "p", "k8s.namespace.name": "ns"}
    assert sdk.detect_resource_attributes(["k8s"], env={"HOSTNAME": "h"}) == {}
