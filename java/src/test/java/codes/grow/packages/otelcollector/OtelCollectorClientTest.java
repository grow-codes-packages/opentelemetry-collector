package codes.grow.packages.otelcollector;

import static codes.grow.packages.otelcollector.TestSupport.FAST_RETRY;
import static codes.grow.packages.otelcollector.TestSupport.closedPort;
import static codes.grow.packages.otelcollector.TestSupport.grpc;
import static codes.grow.packages.otelcollector.TestSupport.grpcServer;
import static codes.grow.packages.otelcollector.TestSupport.grpcUrl;
import static codes.grow.packages.otelcollector.TestSupport.replay;
import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;

import codes.grow.packages.otelcollector.core.AuthenticationException;
import codes.grow.packages.otelcollector.core.CancelledException;
import codes.grow.packages.otelcollector.core.EnvReader;
import codes.grow.packages.otelcollector.core.HealthStatus;
import codes.grow.packages.otelcollector.core.Hooks;
import codes.grow.packages.otelcollector.core.Logger;
import codes.grow.packages.otelcollector.core.MockServer;
import codes.grow.packages.otelcollector.core.NotFoundException;
import codes.grow.packages.otelcollector.core.PermissionDeniedException;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.core.UnavailableException;
import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.logging.BatchOptions;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.LogInput;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import java.time.Duration;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import mockwebserver3.MockWebServer;
import mockwebserver3.RecordedRequest;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class OtelCollectorClientTest {
  private MockServer server;

  @BeforeEach
  void start() throws Exception {
    server = MockServer.start();
  }

  @AfterEach
  void stop() {
    server.close();
  }

  private OtelCollectorClient.Builder builder() {
    return OtelCollectorClient.builder()
        .url(server.url())
        .healthUrl(server.url() + "/")
        .zpagesUrl(server.url())
        .retry(FAST_RETRY);
  }

  @Test
  void healthFromHealthCheckFixture() {
    server.enqueue(replay("health.ok"));
    try (OtelCollectorClient c = builder().build()) {
      HealthStatus h = c.health();
      assertThat(h.healthy()).isTrue();
      assertThat(h.status()).isEqualTo("ok");
      assertThat(h.details())
          .containsEntry("source", "health_check")
          .containsEntry("vendorStatus", "Server available")
          .containsKey("upSince")
          .containsKey("uptime");
      assertThat(server.requests().get(0).uri()).isEqualTo("/");
    }
  }

  @Test
  void health503IsUnhealthyAndNotJsonIsTolerated() {
    server.enqueue(503, "{\"status\":\"Server not available\"}");
    server.enqueue(500, "oops", "Content-Type", "text/plain");
    try (OtelCollectorClient c = builder().build()) {
      HealthStatus h = c.health();
      assertThat(h.healthy()).isFalse();
      assertThat(h.status()).isEqualTo("unhealthy");
      assertThat(h.details())
          .containsEntry("httpStatus", "503")
          .containsEntry("vendorStatus", "Server not available");
      HealthStatus h2 = c.healthAsync().join();
      assertThat(h2.status()).isEqualTo("unhealthy");
      assertThat(h2.details()).containsEntry("httpStatus", "500");
    }
  }

  @Test
  void unreachableHealthFallsBackToZpages() {
    server.enqueue(replay("serverVersion.zpages_stock"));
    try (OtelCollectorClient c =
        builder().healthUrl("http://127.0.0.1:" + closedPort() + "/").build()) {
      HealthStatus h = c.health();
      assertThat(h.healthy()).isTrue();
      assertThat(h.details()).containsEntry("source", "zpages").containsKey("healthCheckError");
      assertThat(server.requests().get(0).uri()).isEqualTo("/debug/servicez");
    }
    server.enqueue(404, "404 page not found", "Content-Type", "text/plain");
    try (OtelCollectorClient c =
        builder().healthUrl("http://127.0.0.1:" + closedPort() + "/").build()) {
      HealthStatus h = c.health();
      assertThat(h.healthy()).isFalse();
      assertThat(h.status()).isEqualTo("unreachable");
    }
  }

  @Test
  void waitUntilReadyPollsAndTimesOut() {
    server.enqueue(503, "{}").enqueue(replay("health.ok"));
    try (OtelCollectorClient c = builder().build()) {
      assertThat(c.waitUntilReady(Duration.ofSeconds(10)).healthy()).isTrue();
      server.enqueue(503, "{}").enqueue(200, "{\"status\":\"Server available\"}");
      assertThat(c.waitUntilReadyAsync(Duration.ofSeconds(10)).join().healthy()).isTrue();
      server.fallback(ex -> MockServer.send(ex, 503, "{}"));
      assertThatThrownBy(() -> c.waitUntilReady(Duration.ofMillis(300)))
          .isInstanceOf(TimeoutException.class);
      CollectorHealthClient h = c.collectorHealth();
      assertThat(h.config()).isSameAs(c.config());
      assertThatThrownBy(() -> h.waitUntilReady(Duration.ofMillis(200)))
          .isInstanceOf(TimeoutException.class);
    }
  }

  @Test
  void standaloneHealthClient() {
    server.enqueue(replay("health.ok"));
    OtelCollectorConfig cfg =
        OtelCollectorConfig.builder()
            .url(server.url())
            .healthUrl(server.url() + "/")
            .zpagesUrl(server.url())
            .build();
    try (CollectorHealthClient h = CollectorHealthClient.create(cfg)) {
      assertThat(h.health().healthy()).isTrue();
      assertThat(h.waitUntilReady().healthy()).isTrue();
    }
  }

  @Test
  void pingHttpSendsEmptyProtobufExport() {
    server.enqueue(replay("ping.http_empty_export"));
    try (OtelCollectorClient c = builder().header("Authorization", "Bearer s3cr3t-token").build()) {
      Duration d = c.ping();
      assertThat(d).isPositive();
      MockServer.Recorded r = server.requests().get(0);
      assertThat(r.method()).isEqualTo("POST");
      assertThat(r.uri()).isEqualTo("/v1/traces");
      assertThat(r.header("Content-Type")).isEqualTo("application/x-protobuf");
      assertThat(r.header("Authorization")).isEqualTo("Bearer s3cr3t-token");
      assertThat(r.body()).isEmpty();
    }
  }

  @Test
  void pingMapsStatusesWithRpcStatusText() {
    server
        .enqueue(replay("ping.http_unauthenticated_missing_token"))
        .enqueue(TestSupport.bytes(403, "08071206666f7262696421"))
        .enqueue(replay("export.http_not_found"))
        .enqueue(503, "{}")
        .enqueue(503, "{}")
        .enqueue(503, "{}");
    try (OtelCollectorClient c =
        builder().header("Authorization", "Bearer wrong-tok3n-value").build()) {
      assertThatThrownBy(c::ping)
          .isInstanceOf(AuthenticationException.class)
          .hasMessageContaining("missing or empty authorization header")
          .hasMessageContaining("HTTP 401");
      assertThatThrownBy(c::ping)
          .isInstanceOf(PermissionDeniedException.class)
          .hasMessageContaining("forbid");
      assertThatThrownBy(c::ping)
          .isInstanceOf(NotFoundException.class)
          .hasMessageContaining("404 page not found");
      assertThatThrownBy(c::ping).isInstanceOf(UnavailableException.class);
      assertThat(server.requests()).hasSize(6);
    }
  }

  @Test
  void pingRedactsSecretsFromVendorText() {
    server.enqueue(
        ex ->
            TestSupport.send(
                ex,
                401,
                "text/plain",
                "token wrong-tok3n-value rejected"
                    .getBytes(java.nio.charset.StandardCharsets.UTF_8)));
    try (OtelCollectorClient c =
        builder().header("Authorization", "Bearer wrong-tok3n-value").build()) {
      assertThatThrownBy(c::ping)
          .isInstanceOf(AuthenticationException.class)
          .satisfies(e -> assertThat(e.getMessage()).doesNotContain("wrong-tok3n-value"));
    }
  }

  @Test
  void pingOverGrpc() throws Exception {
    try (MockWebServer grpcServer = grpcServer()) {
      grpcServer.enqueue(grpc(0, null, ""));
      grpcServer.enqueue(grpc(16, "missing or empty authorization header: Authorization", ""));
      grpcServer.enqueue(grpc(12, "unknown method", ""));
      try (OtelCollectorClient c =
          builder()
              .protocol(Protocol.GRPC)
              .grpcUrl(grpcUrl(grpcServer))
              .header("Authorization", "Bearer abc-123-xyz")
              .build()) {
        assertThat(c.ping()).isPositive();
        RecordedRequest r = grpcServer.takeRequest(5, TimeUnit.SECONDS);
        assertThat(r.getTarget())
            .endsWith("opentelemetry.proto.collector.trace.v1.TraceService/Export");
        assertThat(r.getHeaders().get("authorization")).isEqualTo("Bearer abc-123-xyz");
        assertThat(r.getHeaders().get("content-type")).startsWith("application/grpc");
        assertThatThrownBy(c::ping)
            .isInstanceOf(AuthenticationException.class)
            .hasMessageContaining("(gRPC UNAUTHENTICATED)")
            .hasMessageContaining("missing or empty authorization");
        assertThatThrownBy(() -> c.pingAsync().join()).hasCauseInstanceOf(NotFoundException.class);
      }
    }
  }

  @Test
  void pingOverGrpcUnreachable() {
    try (OtelCollectorClient c =
        builder()
            .protocol(Protocol.GRPC)
            .grpcUrl("http://127.0.0.1:" + closedPort())
            .retry(codes.grow.packages.otelcollector.core.RetryPolicy.none())
            .build()) {
      assertThatThrownBy(c::ping)
          .isInstanceOfAny(UnavailableException.class, TimeoutException.class);
    }
  }

  @Test
  void serverVersionFromZpagesThenTargetInfo() {
    server.enqueue(replay("serverVersion.zpages"));
    try (OtelCollectorClient c = builder().build()) {
      assertThat(c.serverVersion()).isEqualTo("0.165.0");
    }
    server
        .enqueue(404, "404 page not found", "Content-Type", "text/plain")
        .enqueue(replay("serverVersion.telemetry_target_info"));
    try (OtelCollectorClient c = builder().telemetryUrl(server.url() + "/metrics").build()) {
      assertThat(c.serverVersionAsync().join()).isEqualTo("0.165.0");
    }
    server.enqueue(200, "<html>no version</html>", "Content-Type", "text/html");
    try (OtelCollectorClient c = builder().build()) {
      assertThatThrownBy(c::serverVersion).isInstanceOf(UnsupportedException.class);
    }
    try (OtelCollectorClient c =
        builder()
            .zpagesUrl("http://127.0.0.1:" + closedPort())
            .telemetryUrl("http://127.0.0.1:" + closedPort() + "/metrics")
            .retry(codes.grow.packages.otelcollector.core.RetryPolicy.none())
            .build()) {
      assertThatThrownBy(c::serverVersion).isInstanceOf(UnsupportedException.class);
    }
  }

  @Test
  void parsers() {
    assertThat(CollectorHealthClient.parseServicezVersion("nothing")).isEmpty();
    assertThat(CollectorHealthClient.parseTargetInfoVersion("x 1\n")).isEmpty();
    assertThat(
            CollectorHealthClient.parseTargetInfoVersion(
                "# HELP\ntarget_info{service_name=\"otelcol\",service_version=\"1.2.3\"} 1\n"))
        .contains("1.2.3");
  }

  @Test
  void queryClientsAreUnsupported() {
    try (OtelCollectorClient c = builder().build()) {
      assertThatThrownBy(() -> c.traceQuery().getTrace("4bf92f3577b34da6a3ce929d0e0e4736"))
          .isInstanceOf(UnsupportedException.class)
          .hasMessageContaining("no query API");
      assertThatThrownBy(() -> c.traceQuery().searchTraces(null))
          .isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.traceQuery().listServices())
          .isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.traceQuery().listOperations("x"))
          .isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.traceQuery().getDependencies(null, null))
          .isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.traceQuery().waitForTrace("x", null))
          .isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.logQuery().search("x")).isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.logQuery().waitForLog("x"))
          .isInstanceOf(UnsupportedException.class);
      assertThatThrownBy(() -> c.logQuery().listLabels()).isInstanceOf(UnsupportedException.class);
    }
  }

  @Test
  void ownedResourcesCloseWithTheClient() {
    server.fallback(ex -> TestSupport.send(ex, 200, "application/x-protobuf", new byte[0]));
    OtelCollectorClient c = builder().serviceName("svc-owned").build();
    BatchingLogClient lc = c.logClient();
    BatchingLogClient lc2 = c.logClient(BatchOptions.defaults());
    OtlpLogExporter exp = c.logExporter();
    TracingHandle th = c.initTracing(TracingConfig.builder("explicit").setGlobal(false).build());
    TelemetryHandle tel =
        c.initTelemetry(TelemetryOptions.builder().setGlobal(false).metrics(false).build());
    lc.send(LogInput.of("hello"));
    assertThat(exp.endpoint()).isEqualTo(server.url() + "/v1/logs");
    assertThat(c.toString()).contains("OtelCollectorClient");
    assertThat(c.transport()).isNotNull();
    c.close();
    c.close();
    assertThat(c.isClosed()).isTrue();
    assertThat(th.isShutdown()).isTrue();
    assertThat(tel.isShutdown()).isTrue();
    assertThat(lc.stats().sent()).isEqualTo(1);
    assertThat(lc2.stats().sent()).isZero();
    assertThatThrownBy(() -> c.register(() -> {})).isInstanceOf(CancelledException.class);
    assertThatThrownBy(c::ping).isInstanceOf(CancelledException.class);
    assertThatThrownBy(c::serverVersion).isInstanceOf(CancelledException.class);
    assertThatThrownBy(() -> c.serverVersionAsync().join())
        .hasCauseInstanceOf(CancelledException.class);
    assertThatThrownBy(c::health).isInstanceOf(CancelledException.class);
  }

  @Test
  void initTracingFromClientConfig() {
    server.fallback(ex -> TestSupport.send(ex, 200, "application/x-protobuf", new byte[0]));
    try (OtelCollectorClient c =
        OtelCollectorClient.builder()
            .url(server.url())
            .serviceName("svc-tracing")
            .fromEnv(EnvReader.of(Map.of()))
            .build()) {
      TracingHandle h = c.initTracing();
      assertThat(h.config().endpoint()).contains(server.url() + "/v1/traces");
      assertThat(h.config().serviceName()).isEqualTo("svc-tracing");
      h.shutdown();
    }
  }

  @Test
  void builderSettersReachTheConfig() {
    try (OtelCollectorClient c =
        OtelCollectorClient.builder()
            .url("http://collector:4318")
            .connectTimeout(Duration.ofSeconds(1))
            .requestTimeout(Duration.ofSeconds(2))
            .totalTimeout(Duration.ofSeconds(3))
            .tls(TlsOptions.defaults())
            .proxy("http://proxy:3128")
            .userAgentSuffix("app/1")
            .hooks(Hooks.NONE)
            .logger(Logger.NOOP)
            .build()) {
      OtelCollectorConfig cfg = c.config();
      assertThat(cfg.grpcUrl()).isEqualTo("http://collector:4317");
      assertThat(cfg.healthUrl()).isEqualTo("http://collector:13133/");
      assertThat(cfg.zpagesUrl()).isEqualTo("http://collector:55679");
      assertThat(cfg.connectTimeout()).hasSeconds(1);
      assertThat(cfg.requestTimeout()).hasSeconds(2);
      assertThat(cfg.totalTimeout()).hasSeconds(3);
      assertThat(cfg.proxy()).contains("http://proxy:3128");
      assertThat(cfg.userAgentSuffix()).contains("app/1");
    }
    assertThatThrownBy(() -> OtelCollectorClient.builder().url("ftp://x").build())
        .isInstanceOf(codes.grow.packages.otelcollector.core.ConfigException.class);
  }
}
