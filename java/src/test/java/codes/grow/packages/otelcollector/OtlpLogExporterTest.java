package codes.grow.packages.otelcollector;

import static codes.grow.packages.otelcollector.TestSupport.FAST_RETRY;
import static codes.grow.packages.otelcollector.TestSupport.bytes;
import static codes.grow.packages.otelcollector.TestSupport.closedPort;
import static codes.grow.packages.otelcollector.TestSupport.grpc;
import static codes.grow.packages.otelcollector.TestSupport.grpcServer;
import static codes.grow.packages.otelcollector.TestSupport.grpcUrl;
import static codes.grow.packages.otelcollector.TestSupport.replay;
import static org.assertj.core.api.Assertions.assertThat;

import codes.grow.packages.otelcollector.core.AuthenticationException;
import codes.grow.packages.otelcollector.core.CancelledException;
import codes.grow.packages.otelcollector.core.MockServer;
import codes.grow.packages.otelcollector.core.NotFoundException;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.UnavailableException;
import codes.grow.packages.otelcollector.core.ValidationException;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.ExportOptions;
import codes.grow.packages.otelcollector.logging.ExportResult;
import codes.grow.packages.otelcollector.logging.LogInput;
import codes.grow.packages.otelcollector.logging.LogRecord;
import codes.grow.packages.otelcollector.logging.Severity;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import java.time.Duration;
import java.time.Instant;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import mockwebserver3.MockWebServer;
import mockwebserver3.RecordedRequest;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class OtlpLogExporterTest {
  private static final ExportOptions GZIP = new ExportOptions(true, Duration.ofSeconds(10));
  private static final ExportOptions PLAIN = new ExportOptions(false, Duration.ofSeconds(10));

  private MockServer server;

  @BeforeAll
  static void quiet() {
    TestSupport.quietOtelLogs();
  }

  @BeforeEach
  void start() throws Exception {
    server = MockServer.start();
  }

  @AfterEach
  void stop() {
    server.close();
  }

  private OtelCollectorConfig.Builder cfg() {
    return OtelCollectorConfig.builder().url(server.url()).retry(FAST_RETRY);
  }

  private static LogRecord record(String body) {
    return LogRecord.builder()
        .timestamp(Instant.parse("2026-11-18T20:00:00Z"))
        .observedTimestamp(Instant.parse("2026-11-18T20:00:01Z"))
        .severity(Severity.ERROR)
        .body(body)
        .attributes(
            Map.of(
                "user.id",
                42,
                "ratio",
                0.5,
                "ok",
                true,
                "tags",
                List.of("a", "b"),
                "nums",
                List.of(1, 2),
                "flags",
                List.of(true),
                "ds",
                List.of(1.5),
                "mixed",
                List.of("a", 1),
                "nested",
                Map.of("k", "v", "l", List.of(1.0)),
                "empty",
                List.of()))
        .resource(Map.of("service.name", "svc"))
        .traceId("4bf92f3577b34da6a3ce929d0e0e4736")
        .spanId("00f067aa0ba902b7")
        .traceFlags(1)
        .build();
  }

  @Test
  void httpExportSucceeds() {
    server
        .enqueue(replay("exportLogs.http_protobuf_ok"))
        .enqueue(replay("exportLogs.http_protobuf_ok"));
    OtlpLogExporter exp = new OtlpLogExporter(cfg().header("Authorization", "Bearer abc").build());
    ExportResult r = exp.export(List.of(record("hello"), record("again")), GZIP).join();
    assertThat(r.accepted()).isEqualTo(2);
    assertThat(r.rejected()).isZero();
    assertThat(r.error()).isEmpty();
    LogRecord noTrace = LogRecord.builder().body("plain").severity(Severity.INFO).build();
    assertThat(exp.export(List.of(noTrace), PLAIN).join().accepted()).isEqualTo(1);
    MockServer.Recorded req = server.requests().get(0);
    assertThat(req.uri()).isEqualTo("/v1/logs");
    assertThat(req.header("Content-Type")).isEqualTo("application/x-protobuf");
    assertThat(req.header("Content-Encoding")).isEqualTo("gzip");
    assertThat(req.header("Authorization")).isEqualTo("Bearer abc");
    assertThat(server.requests().get(1).header("Content-Encoding")).isNull();
    assertThat(exp.export(List.of(), GZIP).join().accepted()).isZero();
    exp.shutdown().join();
    exp.shutdown().join();
    ExportResult after = exp.export(List.of(record("x")), GZIP).join();
    assertThat(after.rejected()).isEqualTo(1);
    assertThat(after.error()).containsInstanceOf(CancelledException.class);
  }

  @Test
  void partialSuccessReportsRejected() {
    server.enqueue(bytes(200, "0a0e0802120a72656a65637465642121"));
    OtlpLogExporter exp = new OtlpLogExporter(cfg().compression(Compression.NONE).build());
    ExportResult r = exp.export(List.of(record("a"), record("b"), record("c")), GZIP).join();
    assertThat(r.accepted()).isEqualTo(1);
    assertThat(r.rejected()).isEqualTo(2);
    assertThat(r.error().get())
        .isInstanceOf(ValidationException.class)
        .hasMessageContaining("rejected 2 of 3")
        .hasMessageContaining("rejected!!");
    server.enqueue(bytes(200, "0a020802"));
    ExportResult r2 = exp.export(List.of(record("a"), record("b"), record("c")), GZIP).join();
    assertThat(r2.rejected()).isEqualTo(2);
  }

  @Test
  void httpErrorsAreMapped() {
    server
        .enqueue(replay("ping.http_unauthenticated_missing_token"))
        .enqueue(replay("export.http_payload_too_large"))
        .enqueue(replay("export.http_not_found"));
    OtlpLogExporter exp = new OtlpLogExporter(cfg().retry(RetryPolicy.none()).build());
    ExportResult r = exp.export(List.of(record("x")), GZIP).join();
    assertThat(r.rejected()).isEqualTo(1);
    assertThat(r.error().get())
        .isInstanceOf(AuthenticationException.class)
        .hasMessageContaining("authorization");
    assertThat(exp.export(List.of(record("x")), GZIP).join().error().get())
        .isInstanceOf(ValidationException.class)
        .hasMessageContaining("request body too large");
    assertThat(exp.export(List.of(record("x")), GZIP).join().error().get())
        .isInstanceOf(NotFoundException.class);
  }

  @Test
  void retryableStatusIsRetriedByTheExporter() {
    server.enqueue(503, "{}").enqueue(replay("exportLogs.http_protobuf_ok"));
    OtlpLogExporter exp = new OtlpLogExporter(cfg().build());
    assertThat(exp.export(List.of(record("x")), GZIP).join().accepted()).isEqualTo(1);
    assertThat(server.requests()).hasSize(2);
  }

  @Test
  void networkErrorAndTimeout() {
    OtlpLogExporter exp =
        new OtlpLogExporter(
            OtelCollectorConfig.builder()
                .url("http://127.0.0.1:" + closedPort())
                .retry(RetryPolicy.none())
                .build());
    ExportResult r = exp.export(List.of(record("x")), GZIP).join();
    assertThat(r.error().get()).isInstanceOfAny(UnavailableException.class, TimeoutException.class);
    server.enqueue(
        ex -> {
          Thread.sleep(1500);
          MockServer.send(ex, 200, "");
        });
    OtlpLogExporter slow = new OtlpLogExporter(cfg().build());
    ExportResult t =
        slow.export(List.of(record("x")), new ExportOptions(true, Duration.ofMillis(100))).join();
    assertThat(t.error().get()).isInstanceOf(TimeoutException.class);
    slow.shutdown().join();
  }

  @Test
  void grpcExport() throws Exception {
    try (MockWebServer g = grpcServer()) {
      g.enqueue(grpc(0, null, ""));
      g.enqueue(grpc(0, null, "0a050801120178"));
      g.enqueue(grpc(16, "missing or empty authorization header", ""));
      g.enqueue(grpc(13, null, ""));
      OtlpLogExporter exp =
          new OtlpLogExporter(
              OtelCollectorConfig.builder()
                  .protocol(Protocol.GRPC)
                  .grpcUrl(grpcUrl(g))
                  .retry(RetryPolicy.none())
                  .build());
      assertThat(exp.endpoint()).isEqualTo(grpcUrl(g));
      assertThat(exp.export(List.of(record("a")), GZIP).join().accepted()).isEqualTo(1);
      RecordedRequest req = g.takeRequest(5, TimeUnit.SECONDS);
      assertThat(req.getTarget()).endsWith("LogsService/Export");
      ExportResult partial = exp.export(List.of(record("a"), record("b")), GZIP).join();
      assertThat(partial.accepted()).isEqualTo(1);
      assertThat(partial.error().get()).hasMessageContaining("(x)");
      assertThat(exp.export(List.of(record("a")), GZIP).join().error().get())
          .isInstanceOf(AuthenticationException.class)
          .hasMessageContaining("gRPC UNAUTHENTICATED");
      assertThat(exp.export(List.of(record("a")), GZIP).join().error().get())
          .isInstanceOf(ValidationException.class)
          .hasMessageContaining("internal");
      exp.shutdown().join();
    }
  }

  @Test
  void exportsAreSerialized() {
    server.fallback(replay("exportLogs.http_protobuf_ok"));
    OtlpLogExporter exp = new OtlpLogExporter(cfg().build());
    CompletableFuture<ExportResult> a = exp.export(List.of(record("1")), GZIP);
    CompletableFuture<ExportResult> b = exp.export(List.of(record("2")), GZIP);
    assertThat(a.join().accepted()).isEqualTo(1);
    assertThat(b.join().accepted()).isEqualTo(1);
    exp.shutdown().join();
  }

  @Test
  void logClientsFromConfig() {
    server.fallback(replay("exportLogs.http_protobuf_ok"));
    OtelCollectorConfig c =
        cfg().serviceName("svc").serviceVersion("1.0").environment("dev").build();
    try (BatchingLogClient lc = OtelCollectorLogs.createLogClient(c)) {
      assertThat(lc.resource())
          .containsEntry("service.name", "svc")
          .containsEntry("service.version", "1.0")
          .containsEntry("deployment.environment.name", "dev");
      lc.logger("checkout").warn("hi", Map.of("password", "hunter2"));
      lc.send(LogInput.of("x"));
      lc.flush();
      assertThat(lc.stats().sent()).isEqualTo(2);
    }
    try (BatchingLogClient lc =
        OtelCollectorLogs.createLogClient(c, c.logsBatch().toBuilder().maxBatchSize(1).build())) {
      lc.send(LogInput.of("y"));
    }
  }
}
