package codes.grow.packages.otelcollector.integration;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import codes.grow.packages.otelcollector.CollectorHealthClient;
import codes.grow.packages.otelcollector.InstrumentOptions;
import codes.grow.packages.otelcollector.OtelCollectorClient;
import codes.grow.packages.otelcollector.OtelCollectorConfig;
import codes.grow.packages.otelcollector.OtelCollectorLogs;
import codes.grow.packages.otelcollector.OtelCollectorMetrics;
import codes.grow.packages.otelcollector.OtelCollectorTelemetry;
import codes.grow.packages.otelcollector.OtelCollectorTracing;
import codes.grow.packages.otelcollector.OtlpLogExporter;
import codes.grow.packages.otelcollector.ResourceDetector;
import codes.grow.packages.otelcollector.Signal;
import codes.grow.packages.otelcollector.TelemetryHandle;
import codes.grow.packages.otelcollector.TelemetryOptions;
import codes.grow.packages.otelcollector.core.AuthenticationException;
import codes.grow.packages.otelcollector.core.EnvReader;
import codes.grow.packages.otelcollector.core.HealthStatus;
import codes.grow.packages.otelcollector.core.NotFoundException;
import codes.grow.packages.otelcollector.core.Poll;
import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.core.ValidationException;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.ExportOptions;
import codes.grow.packages.otelcollector.logging.ExportResult;
import codes.grow.packages.otelcollector.logging.LogInput;
import codes.grow.packages.otelcollector.logging.LogRecord;
import codes.grow.packages.otelcollector.logging.Severity;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import codes.grow.packages.otelcollector.tracing.query.Span;
import codes.grow.packages.otelcollector.tracing.query.Trace;
import codes.grow.packages.otelcollector.tracing.query.WaitForTraceOptions;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.UUID;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

/**
 * Integration tests against the real OpenTelemetry Collector from {@code docker/compose.yml}.
 * Every public function of the package is called at least once with a round-trip assertion:
 * telemetry is sent to the collector and read back from the capture files (file exporter), the
 * Jaeger sidecar or the collector's own endpoints.
 */
class CollectorTest {
  // JUL keeps loggers weakly: hold a strong reference or the OFF level is lost after a GC.
  private static final java.util.logging.Logger OTEL_JUL =
      java.util.logging.Logger.getLogger("io.opentelemetry");

  private static Map<String, String> env;

  @BeforeAll
  static void setup() {
    env = Readback.env();
    assumeTrue(
        env.containsKey(EnvReader.DEFAULT_PREFIX + "URL"),
        "GROW_OPENTELEMETRY_COLLECTOR_URL is not set; run through docker/compose.yml");
    // expected export failures below are logged by the SDK through JUL from OkHttp threads
    OTEL_JUL.setLevel(java.util.logging.Level.OFF);
  }

  private static String run() {
    return "java-" + UUID.randomUUID().toString().substring(0, 8);
  }

  private static OtelCollectorConfig.Builder cfg() {
    return OtelCollectorConfig.builder().fromEnv(EnvReader.of(env));
  }

  private static OtelCollectorClient client(OtelCollectorConfig.Builder b) {
    return OtelCollectorClient.create(b.build());
  }

  private static List<LogRecord> waitLogs(String marker, int min) {
    return Poll.until(
        "capture logs " + marker,
        Duration.ofSeconds(30),
        Duration.ofMillis(250),
        () -> {
          List<LogRecord> hits = new ArrayList<>();
          for (LogRecord r : Readback.captureLogs()) {
            if (r.body().contains(marker)) {
              hits.add(r);
            }
          }
          return hits.size() >= min ? Optional.of(hits) : Optional.empty();
        });
  }

  private static String waitCapture(String signal, String... needles) {
    return Poll.until(
        "capture " + signal,
        Duration.ofSeconds(30),
        Duration.ofMillis(250),
        () -> {
          String text = Readback.captureText(signal);
          for (String n : needles) {
            if (!text.contains(n)) {
              return Optional.empty();
            }
          }
          return Optional.of(text);
        });
  }

  private static Trace waitTrace(String traceId, int minSpans) {
    return new Readback.JaegerTraceQuery()
        .waitForTrace(
            traceId, new WaitForTraceOptions(Duration.ofSeconds(30), Duration.ofMillis(250), minSpans));
  }

  private static LogRecord byBody(List<LogRecord> recs, String body) {
    return recs.stream().filter(r -> r.body().equals(body)).findFirst().orElseThrow();
  }

  @Test
  void clientAgainstCollector() {
    String version = env.get("OTELCOL_TEST_SERVER_VERSION");
    OtelCollectorClient c = OtelCollectorClient.builder().fromEnv(EnvReader.of(env)).build();
    {
      HealthStatus h = c.health();
      assertTrue(h.healthy(), h.toString());
      assertEquals("ok", h.status());
      assertEquals("Server available", h.details().get("vendorStatus"));
      assertEquals("health_check", h.details().get("source"));
      assertTrue(c.healthAsync().join().healthy());
      assertTrue(c.waitUntilReady(Duration.ofSeconds(10)).healthy());
      assertTrue(c.waitUntilReadyAsync(Duration.ofSeconds(10)).join().healthy());
      assertTrue(c.collectorHealth().health().healthy());
      assertFalse(c.ping().isNegative());
      assertNotNull(c.pingAsync().join());
      assertEquals(version, c.serverVersion());
      assertEquals(version, c.serverVersionAsync().join());
      assertThrows(
          UnsupportedException.class,
          () -> c.traceQuery().getTrace(env.get("OTELCOL_TEST_BOOTSTRAP_TRACE_ID")));
      assertThrows(UnsupportedException.class, () -> c.logQuery().search("x"));
      c.register(() -> {});
      c.close();
      c.close();
      assertTrue(c.isClosed());
    }
    // target_info fallback: zpages pointed at a path without the page
    try (OtelCollectorClient fb =
        client(cfg().zpagesUrl(env.get("OTELCOL_TEST_JAEGER_URL") + "/nope"))) {
      assertEquals(version, fb.serverVersion());
    }
    // gRPC ping
    try (OtelCollectorClient g = client(cfg().protocol(Protocol.GRPC))) {
      assertFalse(g.ping().isNegative());
    }
    // process environment only (GROW_OPENTELEMETRY_COLLECTOR_URL)
    try (OtelCollectorClient p = OtelCollectorClient.fromEnv()) {
      assertTrue(p.health().healthy());
    }
    try (CollectorHealthClient h = CollectorHealthClient.fromEnv()) {
      assertTrue(h.waitUntilReady(Duration.ofSeconds(10)).healthy());
    }
    try (CollectorHealthClient h = CollectorHealthClient.create(cfg().build())) {
      assertEquals(version, h.serverVersion());
    }
  }

  @Test
  void authAndErrors() {
    String token = env.get("OTELCOL_TEST_BEARER_TOKEN");
    String authUrl = env.get("OTELCOL_TEST_AUTH_URL");
    String authGrpc = env.get("OTELCOL_TEST_AUTH_GRPC_URL");
    try (OtelCollectorClient noAuth = client(OtelCollectorConfig.builder().url(authUrl))) {
      AuthenticationException e = assertThrows(AuthenticationException.class, noAuth::ping);
      assertTrue(e.getMessage().contains("authorization"), e.getMessage());
      OtlpLogExporter exp = noAuth.logExporter();
      ExportResult r =
          exp.export(
                  List.of(LogRecord.builder().body("x").build()),
                  new ExportOptions(true, Duration.ofSeconds(10)))
              .join();
      assertInstanceOf(AuthenticationException.class, r.error().orElseThrow());
    }
    try (OtelCollectorClient wrong =
        client(OtelCollectorConfig.builder().url(authUrl).header("Authorization", "Bearer wrong-" + token))) {
      AuthenticationException e = assertThrows(AuthenticationException.class, wrong::ping);
      assertFalse(e.getMessage().contains(token));
    }
    try (OtelCollectorClient ok =
        client(OtelCollectorConfig.builder().url(authUrl).header("Authorization", "Bearer " + token))) {
      ok.ping();
      ExportResult big =
          ok.logExporter()
              .export(
                  List.of(LogRecord.builder().body("x".repeat(100_000)).build()),
                  new ExportOptions(false, Duration.ofSeconds(10)))
              .join();
      assertInstanceOf(ValidationException.class, big.error().orElseThrow());
    }
    try (OtelCollectorClient g =
        client(OtelCollectorConfig.builder().protocol(Protocol.GRPC).grpcUrl(authGrpc))) {
      assertThrows(AuthenticationException.class, g::ping);
    }
    try (OtelCollectorClient g =
        client(
            OtelCollectorConfig.builder()
                .protocol(Protocol.GRPC)
                .grpcUrl(authGrpc)
                .header("Authorization", "Bearer " + token))) {
      g.ping();
    }
    try (OtelCollectorClient wrongPath =
        client(
            cfg()
                .signalUrl(
                    Signal.TRACES, env.get(EnvReader.DEFAULT_PREFIX + "URL") + "/v1/nope"))) {
      assertThrows(NotFoundException.class, wrongPath::ping);
    }
  }

  @Test
  void stockCollector() {
    String stock = env.get("OTELCOL_TEST_STOCK_URL");
    try (OtelCollectorClient c = client(OtelCollectorConfig.builder().url(stock))) {
      HealthStatus h = c.health();
      assertTrue(h.healthy(), h.toString());
      assertEquals("zpages", h.details().get("source"));
      assertEquals(env.get("OTELCOL_TEST_SERVER_VERSION"), c.serverVersion());
      c.ping();
      BatchingLogClient lc = c.logClient();
      lc.send(LogInput.of("stock " + run()));
      lc.flush();
      assertEquals(1, lc.stats().sent());
    }
    try (CollectorHealthClient h =
        CollectorHealthClient.create(
            OtelCollectorConfig.builder()
                .url(stock)
                .healthUrl(env.get("OTELCOL_TEST_STOCK_HEALTH_URL"))
                .build())) {
      assertEquals("zpages", h.waitUntilReady(Duration.ofSeconds(5)).details().get("source"));
    }
  }

  @Test
  void logExporterAndLogClient() {
    String run = run();
    for (Protocol protocol : List.of(Protocol.HTTP_PROTOBUF, Protocol.GRPC)) {
      try (OtelCollectorClient c = client(cfg().protocol(protocol))) {
        OtlpLogExporter exp = c.logExporter();
        String marker = "exporter " + protocol.wireValue() + " " + run;
        ExportResult r =
            exp.export(
                    List.of(
                        LogRecord.builder()
                            .timestamp(Instant.now())
                            .severity(Severity.ERROR)
                            .body(marker)
                            .attributes(
                                Map.of("grow.run", run, "user.id", 42, "nested", Map.of("k", "v")))
                            .resource(Map.of("service.name", "java-exporter-" + run))
                            .traceId("4bf92f3577b34da6a3ce929d0e0e4736")
                            .spanId("00f067aa0ba902b7")
                            .traceFlags(1)
                            .build()),
                    new ExportOptions(true, Duration.ofSeconds(10)))
                .join();
        assertEquals(1, r.accepted(), String.valueOf(r));
        LogRecord got = waitLogs(marker, 1).get(0);
        assertEquals(Severity.ERROR, got.severity());
        assertEquals(Optional.of("4bf92f3577b34da6a3ce929d0e0e4736"), got.traceId());
        assertEquals(Optional.of("00f067aa0ba902b7"), got.spanId());
        assertEquals(42L, got.attributes().get("user.id"));
        assertEquals(Map.of("k", "v"), got.attributes().get("nested"));
        assertEquals("java-exporter-" + run, got.resource().get("service.name"));
        exp.shutdown().join();
      }
    }
    try (OtelCollectorClient c = client(cfg().serviceName("java-logs-" + run))) {
      BatchingLogClient lc = c.logClient();
      lc.logger("checkout", Map.of("component", "cart"))
          .warn("logger " + run, Map.of("password", "hunter2-" + run));
      lc.sendBatch(List.of(LogInput.of("batch a " + run), LogInput.of("batch b " + run)));
      lc.flush();
      BatchingLogClient lc2 = c.logClient(c.config().logsBatch());
      lc2.send(LogInput.of("client batch " + run));
      lc2.flush();
      List<LogRecord> recs = waitLogs(run, 6);
      LogRecord logger = byBody(recs, "logger " + run);
      assertEquals("[REDACTED]", logger.attributes().get("password"));
      assertEquals("cart", logger.attributes().get("component"));
      assertEquals("java-logs-" + run, logger.resource().get("service.name"));
      assertEquals(3, lc.stats().sent());
    }
    OtelCollectorConfig config = cfg().build();
    try (BatchingLogClient p = OtelCollectorLogs.createLogClient(config)) {
      p.send(LogInput.of("package client " + run));
    }
    try (BatchingLogClient p =
        OtelCollectorLogs.createLogClient(config, config.logsBatch().toBuilder().maxBatchSize(1).build())) {
      p.send(LogInput.of("package batch " + run));
    }
    try (BatchingLogClient p = OtelCollectorLogs.logClientBuilder(config).build()) {
      p.send(LogInput.of("package builder " + run));
    }
    waitLogs("package ", 3);
  }

  @Test
  void initTelemetryRoundTrip() {
    for (Protocol protocol : List.of(Protocol.HTTP_PROTOBUF, Protocol.GRPC)) {
      String run = run();
      String service = "java-telemetry-" + protocol.wireValue().replace('/', '-') + "-" + run;
      OtelCollectorConfig config =
          cfg()
              .protocol(protocol)
              .serviceName(service)
              .serviceVersion("1.2.3")
              .environment("sdk-test")
              .resourceAttribute("grow.run", run)
              .runtimeMetrics(true)
              .build();
      TelemetryHandle h;
      OtelCollectorClient owner = null;
      if (protocol == Protocol.GRPC) {
        h =
            OtelCollectorTelemetry.initTelemetry(
                config, TelemetryOptions.builder().setGlobal(false).build());
      } else {
        owner = OtelCollectorClient.create(config);
        h = owner.initTelemetry(TelemetryOptions.builder().setGlobal(false).build());
      }
      io.opentelemetry.api.trace.Span root = h.tracer("grow-it").spanBuilder("root-" + run).startSpan();
      io.opentelemetry.api.trace.Span child =
          h.tracer("grow-it", "1.0")
              .spanBuilder("child")
              .setParent(io.opentelemetry.context.Context.current().with(root))
              .startSpan();
      child.end();
      String traceId = root.getSpanContext().getTraceId();
      try (io.opentelemetry.context.Scope scope = root.makeCurrent()) {
        assertNotNull(scope);
        h.loggerProvider()
            .get("bridge")
            .logRecordBuilder()
            .setSeverity(io.opentelemetry.api.logs.Severity.WARN)
            .setBody("telemetry bridge " + run)
            .emit();
      }
      root.end();
      h.counter("grow_it_requests", InstrumentOptions.unit("1")).add(3);
      h.upDownCounter("grow_it_inflight", InstrumentOptions.NONE).add(2);
      h.histogram("grow_it_latency", InstrumentOptions.boundaries(10.0, 100.0)).record(50);
      h.gauge("grow_it_temperature", InstrumentOptions.NONE).set(42.5);
      h.meter("custom").counterBuilder("grow_it_custom").build().add(1);
      h.logger("it").info("telemetry logclient " + run);
      h.logClient().orElseThrow().send(LogInput.of("telemetry direct " + run));
      h.forceFlush();

      Trace tr = waitTrace(traceId, 2);
      assertEquals(service, tr.spans().get(0).serviceName());
      assertEquals(2, tr.spans().size());
      String metrics =
          waitCapture(
              "metrics",
              service,
              "grow_it_requests",
              "grow_it_inflight",
              "grow_it_latency",
              "grow_it_temperature",
              "grow_it_custom",
              "jvm.memory.used");
      assertTrue(metrics.contains("\"explicitBounds\":[10,100]"), "histogram bounds");
      List<LogRecord> recs = waitLogs(run, 3);
      LogRecord bridge = byBody(recs, "telemetry bridge " + run);
      assertEquals(Optional.of(traceId), bridge.traceId());
      assertEquals(service, bridge.resource().get("service.name"));
      assertEquals("sdk-test", bridge.resource().get("deployment.environment.name"));
      assertNotNull(bridge.resource().get("host.name"));
      assertEquals(service, byBody(recs, "telemetry direct " + run).resource().get("service.name"));
      assertTrue(h.tracing().isPresent());
      assertEquals(service, h.config().serviceName());
      h.shutdown();
      if (owner != null) {
        owner.close();
      }
    }
  }

  @Test
  void globalTelemetryAndMetricHelpers() {
    String run = run();
    // process environment only (GROW_OPENTELEMETRY_COLLECTOR_URL); registers the global instance
    TelemetryHandle h = OtelCollectorTelemetry.initTelemetry();
    OtelCollectorMetrics.counter("grow_it_global_" + run.replace('-', '_')).add(1);
    OtelCollectorMetrics.counter("grow_it_global2", InstrumentOptions.unit("1")).add(1);
    OtelCollectorMetrics.upDownCounter("grow_it_global_updown", InstrumentOptions.NONE).add(1);
    OtelCollectorMetrics.histogram("grow_it_global_hist", InstrumentOptions.NONE).record(1);
    OtelCollectorMetrics.gauge("grow_it_global_gauge", InstrumentOptions.NONE).set(1);
    OtelCollectorMetrics.meter().counterBuilder("grow_it_global_meter").build().add(1);
    OtelCollectorMetrics.meter("x").counterBuilder("grow_it_global_named").build().add(1);
    h.forceFlush();
    waitCapture(
        "metrics",
        "grow_it_global_" + run.replace('-', '_'),
        "grow_it_global2",
        "grow_it_global_updown",
        "grow_it_global_hist",
        "grow_it_global_gauge",
        "grow_it_global_meter",
        "grow_it_global_named");
    h.shutdown();
    // a bare metric exporter and resource detection
    try (TelemetryHandle t =
        OtelCollectorTelemetry.initTelemetry(
            cfg().serviceName("java-bare-" + run).build(),
            TelemetryOptions.builder()
                .setGlobal(false)
                .traces(false)
                .logs(false)
                .metricExporter(OtelCollectorMetrics.metricExporter(cfg().build()))
                .resourceDetector(ResourceDetector.system())
                .build())) {
      t.counter("grow_it_bare", InstrumentOptions.NONE).add(1);
      t.forceFlush();
      waitCapture("metrics", "java-bare-" + run, "grow_it_bare");
    }
    OtelCollectorConfig c = cfg().build();
    assertNotNull(OtlpLogExporter.officialExporter(c, true).shutdown());
    // tracing presets that register the global instance (one at a time)
    io.opentelemetry.api.GlobalOpenTelemetry.resetForTest();
    TracingHandle fromEnv = OtelCollectorTracing.initTracing();
    String id = sendSpan(fromEnv, "global-env-" + run);
    assertEquals(fromEnv.config().serviceName(), waitTrace(id, 1).spans().get(0).serviceName());
    fromEnv.shutdown();
    io.opentelemetry.api.GlobalOpenTelemetry.resetForTest();
    TracingHandle fromConfig =
        OtelCollectorTracing.initTracing(cfg().serviceName("java-global-" + run).build());
    id = sendSpan(fromConfig, "global-config-" + run);
    assertEquals("java-global-" + run, waitTrace(id, 1).spans().get(0).serviceName());
    fromConfig.shutdown();
    io.opentelemetry.api.GlobalOpenTelemetry.resetForTest();
  }

  private static String sendSpan(TracingHandle h, String name) {
    io.opentelemetry.api.trace.Span s = h.tracer("it").spanBuilder(name).startSpan();
    String id = s.getSpanContext().getTraceId();
    s.end();
    h.forceFlush();
    return id;
  }

  @Test
  void initTracingRoundTrip() {
    String run = run();
    OtelCollectorConfig config = cfg().serviceName("java-tracing-" + run).build();
    List<TracingHandle> handles = new ArrayList<>();
    handles.add(OtelCollectorTracing.initTracing(
        TracingConfig.builder("java-tracing-a-" + run).setGlobal(false).build(), config));
    handles.add(
        OtelCollectorTracing.initTracing(
            TracingConfig.builder("java-tracing-b-" + run)
                .protocol(Protocol.GRPC)
                .setGlobal(false)
                .build(),
            config));
    try (OtelCollectorClient c = OtelCollectorClient.create(config)) {
      handles.add(
          c.initTracing(TracingConfig.builder("java-tracing-c-" + run).setGlobal(false).build()));
      for (TracingHandle h : handles) {
        io.opentelemetry.api.trace.Span s = h.tracer("it").spanBuilder("span-" + run).startSpan();
        s.addEvent("checkpoint");
        String id = s.getSpanContext().getTraceId();
        s.end();
        h.forceFlush();
        Trace tr = waitTrace(id, 1);
        Span got = tr.spans().get(0);
        assertEquals(h.config().serviceName(), got.serviceName());
        assertEquals("checkpoint", got.events().get(0).name());
        h.shutdown();
      }
    }
    TracingConfig resolved =
        OtelCollectorTracing.resolve(TracingConfig.builder("x").build(), config);
    assertTrue(resolved.endpoint().orElseThrow().endsWith("/v1/traces"));
    assertNotNull(OtelCollectorTracing.spanExporterFactory().create(resolved).shutdown());
    assertNotNull(OtelCollectorTracing.createSpanExporter(resolved).shutdown());
    // explicit config over the process environment
    TracingHandle e1 =
        OtelCollectorTracing.initTracing(
            TracingConfig.builder("java-tracing-env-" + run).setGlobal(false).build());
    e1.shutdown();
  }
}
