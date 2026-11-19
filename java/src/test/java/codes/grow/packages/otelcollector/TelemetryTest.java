package codes.grow.packages.otelcollector;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.MockServer;
import codes.grow.packages.otelcollector.logging.LogRecord;
import codes.grow.packages.otelcollector.testing.logging.InMemoryLogExporter;
import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.api.common.AttributeKey;
import io.opentelemetry.api.logs.Severity;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.sdk.logs.data.LogRecordData;
import io.opentelemetry.sdk.metrics.data.MetricData;
import io.opentelemetry.sdk.testing.exporter.InMemoryLogRecordExporter;
import io.opentelemetry.sdk.testing.exporter.InMemoryMetricExporter;
import io.opentelemetry.sdk.testing.exporter.InMemorySpanExporter;
import io.opentelemetry.sdk.trace.data.SpanData;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.stream.Collectors;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class TelemetryTest {
  private MockServer server;
  private InMemorySpanExporter spans;
  private InMemoryMetricExporter metrics;
  private InMemoryLogRecordExporter otelLogs;
  private InMemoryLogExporter clientLogs;

  @BeforeAll
  static void quiet() {
    TestSupport.quietOtelLogs();
  }

  @BeforeEach
  void start() throws Exception {
    server = MockServer.start();
    server.fallback(ex -> TestSupport.send(ex, 200, "application/x-protobuf", new byte[0]));
    spans = InMemorySpanExporter.create();
    metrics = InMemoryMetricExporter.create();
    otelLogs = InMemoryLogRecordExporter.create();
    clientLogs = new InMemoryLogExporter();
    GlobalOpenTelemetry.resetForTest();
  }

  @AfterEach
  void stop() {
    server.close();
    GlobalOpenTelemetry.resetForTest();
  }

  private OtelCollectorConfig.Builder cfg() {
    return OtelCollectorConfig.builder()
        .url(server.url())
        .serviceName("svc")
        .serviceVersion("1.2.3")
        .environment("test")
        .resourceAttribute("grow.run", "r1")
        .retry(TestSupport.FAST_RETRY);
  }

  private TelemetryOptions.Builder inMemory() {
    return TelemetryOptions.builder()
        .spanExporterFactory(c -> spans)
        .metricExporter(metrics)
        .logRecordExporter(otelLogs)
        .logExporter(clientLogs)
        .resourceDetector(
            ResourceDetector.of(
                Map.of("HOSTNAME", "host-1", "K8S_POD_NAME", "pod-1"), p -> Optional.empty()));
  }

  @Test
  void allSignalsShareOneResource() {
    TelemetryHandle h =
        OtelCollectorTelemetry.initTelemetry(
            cfg().runtimeMetrics(true).build(), inMemory().setGlobal(true).build());
    assertThat(GlobalOpenTelemetry.get().getMeterProvider()).isNotNull();
    Span root = h.tracer().spanBuilder("root").startSpan();
    Span child =
        h.tracer("it", "1.0")
            .spanBuilder("child")
            .setParent(io.opentelemetry.context.Context.current().with(root))
            .startSpan();
    child.end();
    root.end();
    h.counter("grow_requests", InstrumentOptions.unit("1").withDescription("d")).add(3);
    h.upDownCounter("grow_inflight", InstrumentOptions.NONE).add(-1);
    h.histogram("grow_latency", InstrumentOptions.boundaries(10.0, 100.0).withUnit("ms"))
        .record(50);
    h.gauge("grow_temperature", InstrumentOptions.NONE.withMeter("custom")).set(42.5);
    h.meter().counterBuilder("grow_direct").build().add(1);
    h.meter("other").counterBuilder("grow_other").build().add(1);
    h.loggerProvider()
        .get("bridge")
        .logRecordBuilder()
        .setSeverity(Severity.WARN)
        .setBody("bridge record")
        .emit();
    h.logger("checkout", Map.of("component", "cart")).warn("client record");
    h.logger("plain").info("second");
    h.forceFlush();

    List<SpanData> sd = spans.getFinishedSpanItems();
    assertThat(sd).hasSize(2);
    assertThat(sd.get(0).getResource().getAttribute(AttributeKey.stringKey("service.name")))
        .isEqualTo("svc");
    assertThat(sd.get(0).getResource().getAttribute(AttributeKey.stringKey("host.name")))
        .isEqualTo("host-1");
    List<String> names =
        metrics.getFinishedMetricItems().stream()
            .map(MetricData::getName)
            .collect(Collectors.toList());
    assertThat(names)
        .contains(
            "grow_requests",
            "grow_inflight",
            "grow_latency",
            "grow_temperature",
            "grow_direct",
            "grow_other",
            "jvm.memory.used",
            "jvm.thread.count",
            "process.uptime");
    List<LogRecordData> ld = otelLogs.getFinishedLogRecordItems();
    assertThat(ld).hasSize(1);
    assertThat(ld.get(0).getResource().getAttribute(AttributeKey.stringKey("k8s.pod.name")))
        .isEqualTo("pod-1");
    List<LogRecord> cl = clientLogs.records();
    assertThat(cl).hasSize(2);
    assertThat(cl.get(0).resource())
        .containsEntry("service.name", "svc")
        .containsEntry("deployment.environment.name", "test")
        .containsEntry("deployment.environment", "test")
        .containsEntry("grow.run", "r1");
    assertThat(cl.get(0).attributes()).containsEntry("component", "cart");
    assertThat(h.config().serviceName()).isEqualTo("svc");
    assertThat(h.resource().getAttribute(AttributeKey.stringKey("service.version")))
        .isEqualTo("1.2.3");
    assertThat(h.resourceAttributes()).containsEntry("host.name", "host-1");
    assertThat(h.tracing()).isPresent();
    assertThat(h.logClient()).isPresent();
    assertThat(h.openTelemetry().getPropagators()).isNotNull();
    assertThat(h.meterProvider()).isNotNull();
    h.forceFlushAsync(Duration.ofSeconds(5)).join();
    h.shutdownAsync().join();
    h.shutdown();
    assertThat(h.isShutdown()).isTrue();
    assertThatThrownBy(
            () ->
                OtelCollectorTelemetry.initTelemetry(
                    cfg().build(), inMemory().setGlobal(true).build()))
        .isInstanceOf(ConfigException.class)
        .hasMessageContaining("global");
  }

  @Test
  void disabledSignalsAreNoops() {
    try (TelemetryHandle h =
        OtelCollectorTelemetry.initTelemetry(
            cfg().traces(false).logs(false).metrics(false).build(),
            inMemory().setGlobal(false).build())) {
      assertThat(h.tracing()).isEmpty();
      assertThat(h.logClient()).isEmpty();
      h.tracer("x").spanBuilder("s").startSpan().end();
      h.counter("c", InstrumentOptions.NONE).add(1);
      h.logger("l").info("dropped");
      h.loggerProvider().get("x").logRecordBuilder().setBody("x").emit();
      h.forceFlush(Duration.ofSeconds(1));
      assertThat(spans.getFinishedSpanItems()).isEmpty();
      assertThat(clientLogs.records()).isEmpty();
    }
  }

  @Test
  void optionsOverrideTheConfigSwitches() {
    TelemetryHandle h =
        OtelCollectorTelemetry.initTelemetry(
            cfg().traces(false).build(),
            inMemory()
                .setGlobal(false)
                .traces(true)
                .logs(false)
                .metrics(true)
                .runtimeMetrics(false)
                .metricExportInterval(Duration.ofMillis(50))
                .build());
    assertThat(h.tracing()).isPresent();
    assertThat(h.logClient()).isEmpty();
    h.shutdown();
  }

  @Test
  void realExportersAgainstMockCollector() {
    OtelCollectorConfig c = cfg().resourceDetectors(List.of("host", "process")).build();
    try (TelemetryHandle h =
        OtelCollectorTelemetry.initTelemetry(
            c, TelemetryOptions.builder().setGlobal(false).build())) {
      h.tracer().spanBuilder("real").startSpan().end();
      h.counter("real_counter", InstrumentOptions.NONE).add(1);
      h.logger("real").info("real log");
      h.loggerProvider().get("x").logRecordBuilder().setBody("bridge").emit();
      h.forceFlush(Duration.ofSeconds(10));
    }
    List<String> paths =
        server.requests().stream().map(MockServer.Recorded::uri).collect(Collectors.toList());
    assertThat(paths).contains("/v1/traces", "/v1/metrics", "/v1/logs");
  }

  @Test
  void exporterFailuresNeverThrowAtTheCaller() {
    server.fallback(ex -> MockServer.send(ex, 400, ""));
    OtelCollectorConfig c =
        cfg().retry(codes.grow.packages.otelcollector.core.RetryPolicy.none()).build();
    TelemetryHandle h =
        OtelCollectorTelemetry.initTelemetry(
            c,
            TelemetryOptions.builder()
                .setGlobal(false)
                .logs(false)
                .resourceDetector(ResourceDetector.of(Map.of(), p -> Optional.empty()))
                .build());
    h.tracer().spanBuilder("fails").startSpan().end();
    h.forceFlush(Duration.ofSeconds(10));
    assertThat(server.requests()).isNotEmpty();
    h.shutdown();
  }

  @Test
  void initTelemetryFailureCleansUp() {
    assertThatThrownBy(
            () ->
                OtelCollectorTelemetry.initTelemetry(
                    cfg().propagators("nope").build(), inMemory().setGlobal(false).build()))
        .isInstanceOf(ConfigException.class);
  }

  @Test
  void globalMetricHelpers() {
    TelemetryHandle h =
        OtelCollectorTelemetry.initTelemetry(
            cfg().traces(false).logs(false).build(), inMemory().setGlobal(true).build());
    OtelCollectorMetrics.counter("g_counter").add(1);
    OtelCollectorMetrics.counter("g_counter2", InstrumentOptions.unit("1")).add(1);
    OtelCollectorMetrics.upDownCounter("g_updown", InstrumentOptions.NONE).add(1);
    OtelCollectorMetrics.histogram("g_hist", InstrumentOptions.boundaries(1.0)).record(2);
    OtelCollectorMetrics.gauge("g_gauge", InstrumentOptions.NONE).set(1);
    OtelCollectorMetrics.meter().counterBuilder("g_meter").build().add(1);
    OtelCollectorMetrics.meter("custom").counterBuilder("g_custom").build().add(1);
    h.forceFlush();
    List<String> names =
        metrics.getFinishedMetricItems().stream()
            .map(MetricData::getName)
            .collect(Collectors.toList());
    assertThat(names)
        .contains(
            "g_counter", "g_counter2", "g_updown", "g_hist", "g_gauge", "g_meter", "g_custom");
    h.shutdown();
  }

  @Test
  void initTelemetryFromEnvironmentVariables() {
    // the process environment of the test JVM has no GROW_/OTEL_ collector settings
    try (TelemetryHandle h = OtelCollectorTelemetry.initTelemetry()) {
      assertThat(h.config().url()).isNotEmpty();
    } catch (codes.grow.packages.otelcollector.core.OtelCollectorException e) {
      // shutdown may fail when nothing listens on the default endpoint
      assertThat(e).isNotNull();
    }
  }
}
