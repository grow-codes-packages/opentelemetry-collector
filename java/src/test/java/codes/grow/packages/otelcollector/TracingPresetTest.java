package codes.grow.packages.otelcollector;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.MockServer;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.BatchSettings;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingConfig.SamplerType;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import io.opentelemetry.api.common.AttributeKey;
import io.opentelemetry.api.common.Attributes;
import io.opentelemetry.api.common.Value;
import io.opentelemetry.sdk.trace.export.SpanExporter;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class TracingPresetTest {
  private MockServer server;

  @BeforeAll
  static void quiet() {
    TestSupport.quietOtelLogs();
  }

  @BeforeEach
  void start() throws Exception {
    server = MockServer.start();
    server.fallback(ex -> TestSupport.send(ex, 200, "application/x-protobuf", new byte[0]));
  }

  @AfterEach
  void stop() {
    server.close();
  }

  @Test
  void resolveTracesEndpoint() {
    assertThat(OtelCollectorTracing.resolveTracesEndpoint(Protocol.HTTP_PROTOBUF, "http://c:4318"))
        .isEqualTo("http://c:4318/v1/traces");
    assertThat(OtelCollectorTracing.resolveTracesEndpoint(Protocol.HTTP_PROTOBUF, "http://c:4318/"))
        .isEqualTo("http://c:4318/v1/traces");
    assertThat(OtelCollectorTracing.resolveTracesEndpoint(Protocol.HTTP_PROTOBUF, "http://c/x/y"))
        .isEqualTo("http://c/x/y");
    assertThat(OtelCollectorTracing.resolveTracesEndpoint(Protocol.GRPC, "http://c:4317"))
        .isEqualTo("http://c:4317");
  }

  @Test
  void resolveMergesOverCollectorDefaults() {
    OtelCollectorConfig defaults =
        OtelCollectorConfig.builder()
            .url("http://c:4318")
            .protocol(Protocol.GRPC)
            .header("Authorization", "Bearer d")
            .resourceAttribute("team", "core")
            .serviceVersion("1")
            .environment("dev")
            .sampler(SamplerType.TRACEIDRATIO)
            .samplerRatio(0.5)
            .propagators("b3")
            .compression(Compression.NONE)
            .tls(TlsOptions.builder().insecureSkipVerify(true).build())
            .tracesBatch(new BatchSettings(10, 5, Duration.ofMillis(100), Duration.ofSeconds(1)))
            .build();
    TracingConfig r =
        OtelCollectorTracing.resolve(
            TracingConfig.builder("svc")
                .header("x", "1")
                .resourceAttributes(Map.of("a", 1))
                .build(),
            defaults);
    assertThat(r.protocol()).isEqualTo(Protocol.GRPC);
    assertThat(r.endpoint()).contains("http://c:4317");
    assertThat(r.headers()).containsEntry("Authorization", "Bearer d").containsEntry("x", "1");
    assertThat(r.resourceAttributes()).containsEntry("team", "core").containsEntry("a", 1);
    assertThat(r.serviceVersion()).contains("1");
    assertThat(r.environment()).contains("dev");
    assertThat(r.sampler()).isEqualTo(SamplerType.TRACEIDRATIO);
    assertThat(r.samplerRatio()).isEqualTo(0.5);
    assertThat(r.propagators()).containsExactly("b3");
    assertThat(r.compression()).isEqualTo(Compression.NONE);
    assertThat(r.tls()).isPresent();
    assertThat(r.batch().maxQueueSize()).isEqualTo(10);

    TracingConfig explicit =
        OtelCollectorTracing.resolve(
            TracingConfig.builder("svc")
                .serviceVersion("2")
                .environment("prod")
                .endpoint("http://e:4318")
                .sampler(SamplerType.ALWAYS_OFF)
                .propagators(List.of("tracecontext"))
                .batch(new BatchSettings(1, 1, Duration.ofSeconds(1), Duration.ofSeconds(1)))
                .tls(TlsOptions.defaults())
                .build(),
            OtelCollectorConfig.builder().url("http://c:4318").build());
    assertThat(explicit.endpoint()).contains("http://e:4318/v1/traces");
    assertThat(explicit.serviceVersion()).contains("2");
    assertThat(explicit.sampler()).isEqualTo(SamplerType.ALWAYS_OFF);
    assertThat(explicit.propagators()).containsExactly("tracecontext");
    assertThat(explicit.batch().maxQueueSize()).isEqualTo(1);

    TracingConfig grpcOverHttp =
        OtelCollectorTracing.resolve(
            TracingConfig.builder("svc").protocol(Protocol.GRPC).build(),
            OtelCollectorConfig.builder().url("http://c:4318").build());
    assertThat(grpcOverHttp.endpoint()).contains("http://c:4317");
    TracingConfig http =
        OtelCollectorTracing.resolve(
            TracingConfig.builder("svc").build(),
            OtelCollectorConfig.builder().url("http://c:4318").build());
    assertThat(http.endpoint()).contains("http://c:4318/v1/traces");
  }

  @Test
  void exportersForEveryProtocol() {
    SpanExporter http =
        OtelCollectorTracing.createSpanExporter(
            TracingConfig.builder("svc").header("a", "b").build());
    http.shutdown();
    SpanExporter grpc =
        OtelCollectorTracing.spanExporterFactory()
            .create(TracingConfig.builder("svc").protocol(Protocol.GRPC).header("a", "b").build());
    grpc.shutdown();
    SpanExporter noRetry =
        OtelCollectorTracing.createSpanExporter(
            TracingConfig.builder("svc").protocol(Protocol.GRPC).build(), RetryPolicy.none());
    noRetry.shutdown();
    OtelCollectorTracing.createSpanExporter(
            TracingConfig.builder("svc").build(), RetryPolicy.none())
        .shutdown();
    assertThatThrownBy(
            () ->
                OtelCollectorTracing.createSpanExporter(
                    TracingConfig.builder("svc").protocol(Protocol.NATIVE).build()))
        .isInstanceOf(ConfigException.class)
        .hasMessageContaining("no native protocol");
  }

  @Test
  void initTracingVariantsExportToTheCollector() {
    OtelCollectorConfig c =
        OtelCollectorConfig.builder().url(server.url()).serviceName("svc-a").build();
    io.opentelemetry.api.GlobalOpenTelemetry.resetForTest();
    TracingHandle a = OtelCollectorTracing.initTracing(c.toBuilder().build());
    assertThat(a.config().setGlobal()).isTrue();
    a.shutdown();
    io.opentelemetry.api.GlobalOpenTelemetry.resetForTest();
    TracingHandle b =
        OtelCollectorTracing.initTracing(
            TracingConfig.builder("svc-b")
                .setGlobal(false)
                .batch(new BatchSettings(10, 5, Duration.ofMillis(50), Duration.ofSeconds(5)))
                .build(),
            c);
    b.tracer("t").spanBuilder("span").startSpan().end();
    b.forceFlush();
    b.shutdown();
    assertThat(server.requests()).extracting(MockServer.Recorded::uri).contains("/v1/traces");
  }

  @Test
  void resourceDetector() {
    ResourceDetector d =
        ResourceDetector.of(
            Map.of(
                "KUBERNETES_SERVICE_HOST", "10.0.0.1",
                "HOSTNAME", "pod-abc",
                "K8S_POD_UID", "uid",
                "NODE_NAME", "node",
                "K8S_CONTAINER_NAME", "app",
                "K8S_DEPLOYMENT_NAME", "dep",
                "K8S_CLUSTER_NAME", "c1"),
            p -> {
              switch (p) {
                case "/proc/self/cgroup":
                  return Optional.of("0::/\n1:cpu:/docker/" + "a".repeat(64));
                case "/var/run/secrets/kubernetes.io/serviceaccount/namespace":
                  return Optional.of("grow\n");
                default:
                  return Optional.empty();
              }
            });
    Map<String, Object> all = d.detect(OtelCollectorConfig.RESOURCE_DETECTORS);
    assertThat(all)
        .containsEntry("host.name", "pod-abc")
        .containsEntry("container.id", "a".repeat(64))
        .containsEntry("k8s.pod.name", "pod-abc")
        .containsEntry("k8s.namespace.name", "grow")
        .containsEntry("k8s.pod.uid", "uid")
        .containsEntry("k8s.node.name", "node")
        .containsEntry("k8s.container.name", "app")
        .containsEntry("k8s.deployment.name", "dep")
        .containsEntry("k8s.cluster.name", "c1")
        .containsKeys("process.pid", "process.runtime.name", "host.arch", "os.type");
    assertThat(d.detect(List.of())).isEmpty();

    ResourceDetector v2 =
        ResourceDetector.of(
            Map.of("POD_NAME", "p", "POD_NAMESPACE", "ns"),
            p ->
                p.equals("/proc/self/mountinfo")
                    ? Optional.of(
                        "1 2 0:1 / / rw\n"
                            + "3 4 0:2 /var/lib/docker/containers/"
                            + "b".repeat(64)
                            + "/hostname /etc/hostname rw\n")
                    : p.equals("/proc/self/cgroup") ? Optional.of("0::/\n") : Optional.empty());
    assertThat(v2.containerId()).contains("b".repeat(64));
    assertThat(v2.detect(List.of("k8s")))
        .containsEntry("k8s.pod.name", "p")
        .containsEntry("k8s.namespace.name", "ns");
    ResourceDetector none = ResourceDetector.of(Map.of(), p -> Optional.empty());
    assertThat(none.containerId()).isEmpty();
    assertThat(none.detect(List.of("k8s"))).isEmpty();
    assertThat(none.detect(List.of("host"))).containsKey("os.version");
    assertThat(ResourceDetector.system().detect(List.of("container"))).isNotNull();
    assertThat(ResourceDetector.arch("x86_64")).isEqualTo("amd64");
    assertThat(ResourceDetector.arch("aarch64")).isEqualTo("arm64");
    assertThat(ResourceDetector.arch("i386")).isEqualTo("x86");
    assertThat(ResourceDetector.arch("s390x")).isEqualTo("s390x");
  }

  @Test
  void otelValues() {
    Attributes a =
        OtelValues.attributes(
            new java.util.LinkedHashMap<String, Object>(
                Map.of("s", "x", "n", 1, "d", 1.5, "b", true, "m", Map.of("k", List.of("v")))));
    assertThat(a.get(AttributeKey.stringKey("s"))).isEqualTo("x");
    assertThat(a.get(AttributeKey.longKey("n"))).isEqualTo(1L);
    assertThat(a.get(AttributeKey.doubleKey("d"))).isEqualTo(1.5);
    assertThat(a.get(AttributeKey.booleanKey("b"))).isTrue();
    assertThat(a.get(AttributeKey.valueKey("m"))).isNotNull();
    java.util.Map<String, Object> withNull = new java.util.HashMap<>();
    withNull.put("x", null);
    assertThat(OtelValues.attributes(withNull).isEmpty()).isTrue();
    assertThat(
            OtelValues.value(
                new Object() {
                  @Override
                  public String toString() {
                    return "obj";
                  }
                }))
        .isEqualTo(Value.of("obj"));
    assertThat(OtelValues.value(true)).isEqualTo(Value.of(true));
  }
}
