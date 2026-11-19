package codes.grow.packages.otelcollector;

import static codes.grow.packages.otelcollector.TestSupport.env;
import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.EnvReader;
import codes.grow.packages.otelcollector.core.Hooks;
import codes.grow.packages.otelcollector.core.Logger;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.logging.BatchOptions;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.BatchSettings;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingConfig.SamplerType;
import java.nio.file.Paths;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class OtelCollectorConfigTest {
  private static final String P = EnvReader.DEFAULT_PREFIX;
  private static final String A = OtelCollectorConfig.ALIAS_ENV_PREFIX;

  @Test
  void defaults() {
    OtelCollectorConfig c = OtelCollectorConfig.fromEnv(Map.of());
    assertThat(c.url()).isEqualTo("http://127.0.0.1:4318");
    assertThat(c.grpcUrl()).isEqualTo("http://127.0.0.1:4317");
    assertThat(c.healthUrl()).isEqualTo("http://127.0.0.1:13133/");
    assertThat(c.zpagesUrl()).isEqualTo("http://127.0.0.1:55679");
    assertThat(c.telemetryUrl()).isEmpty();
    assertThat(c.protocol()).isEqualTo(Protocol.HTTP_PROTOBUF);
    assertThat(c.compression()).isEqualTo(Compression.GZIP);
    assertThat(c.exportTimeout()).isEqualTo(Duration.ofSeconds(10));
    assertThat(c.metricExportInterval()).isEqualTo(Duration.ofSeconds(60));
    assertThat(c.serviceName()).isEqualTo("unknown_service:java");
    assertThat(c.serviceVersion()).isEmpty();
    assertThat(c.environment()).isEmpty();
    assertThat(c.tracesEnabled()).isTrue();
    assertThat(c.logsEnabled()).isTrue();
    assertThat(c.metricsEnabled()).isTrue();
    assertThat(c.runtimeMetrics()).isFalse();
    assertThat(c.sampler()).isEqualTo(SamplerType.PARENTBASED_ALWAYS_ON);
    assertThat(c.samplerRatio()).isEqualTo(1.0);
    assertThat(c.propagators()).containsExactly("tracecontext", "baggage");
    assertThat(c.resourceDetectors()).containsExactly("host", "process", "container", "k8s");
    assertThat(c.tracesBatch().exportTimeout()).isEqualTo(Duration.ofSeconds(10));
    assertThat(c.tracesBatch().maxQueueSize()).isEqualTo(2048);
    assertThat(c.logsBatch().maxBatchSize()).isEqualTo(BatchOptions.defaults().maxBatchSize());
    assertThat(c.signalUrl(Signal.TRACES)).isEqualTo("http://127.0.0.1:4318/v1/traces");
    assertThat(c.endpointFor(Signal.LOGS)).isEqualTo("http://127.0.0.1:4318/v1/logs");
    assertThat(c.explicitSignalUrl(Signal.METRICS)).isEmpty();
    assertThat(c.headers()).isEmpty();
    assertThat(c.signalHeaders(Signal.TRACES)).isEmpty();
    assertThat(c.hooks()).isSameAs(Hooks.NONE);
    assertThat(c.logger()).isSameAs(Logger.NOOP);
    assertThat(c.retry().maxRetries()).isEqualTo(RetryPolicy.DEFAULT_MAX_RETRIES);
    assertThat(c.tls().isCustom()).isFalse();
    assertThat(c.proxy()).isEmpty();
    assertThat(c.userAgentSuffix()).isEmpty();
    assertThat(c.knownSecrets()).isEmpty();
  }

  @Test
  void growVariablesWinOverAliasAndOtel() {
    Map<String, String> e =
        env(
            P + "URL",
            "http://grow:4318",
            A + "URL",
            "http://alias:4318",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://otel:4318",
            P + "PROTOCOL",
            "http/protobuf",
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            "grpc",
            P + "GRPC_URL",
            "http://grow:4317",
            A + "TRACES_URL",
            "http://alias:4318/custom/traces",
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            "http://otel:4318/custom/logs",
            P + "HEADERS",
            "Authorization=Bearer%20grow,x-a=1",
            A + "HEADERS",
            "x-a=2,x-b=2",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "x-c=3,x-a=3",
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            "x-t=t",
            A + "COMPRESSION",
            "none",
            "OTEL_EXPORTER_OTLP_TIMEOUT",
            "2500",
            P + "HEALTH_URL",
            "http://h:1/",
            P + "ZPAGES_URL",
            "http://z:2",
            A + "TELEMETRY_URL",
            "http://t:8888/metrics",
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.name=from-attrs,team=core,region=eu",
            P + "RESOURCE_ATTRIBUTES",
            "region=us",
            A + "SERVICE_VERSION",
            "1.2.3",
            P + "ENVIRONMENT",
            "staging",
            "OTEL_SDK_DISABLED",
            "false",
            "OTEL_LOGS_EXPORTER",
            "none",
            P + "METRICS",
            "false",
            "OTEL_TRACES_SAMPLER",
            "parentbased_traceidratio",
            "OTEL_TRACES_SAMPLER_ARG",
            "0.25",
            "OTEL_PROPAGATORS",
            "tracecontext,b3",
            A + "METRIC_EXPORT_INTERVAL",
            "5s",
            P + "RUNTIME_METRICS",
            "true",
            P + "RESOURCE_DETECTORS",
            "host,process",
            "OTEL_BSP_SCHEDULE_DELAY",
            "100",
            "OTEL_BSP_EXPORT_TIMEOUT",
            "3000",
            "OTEL_BSP_MAX_QUEUE_SIZE",
            "100",
            "OTEL_BSP_MAX_EXPORT_BATCH_SIZE",
            "10",
            "OTEL_BLRP_SCHEDULE_DELAY",
            "200",
            "OTEL_BLRP_EXPORT_TIMEOUT",
            "4000",
            "OTEL_BLRP_MAX_QUEUE_SIZE",
            "50",
            "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE",
            "5",
            "OTEL_EXPORTER_OTLP_CERTIFICATE",
            "/ca.pem",
            P + "CONNECT_TIMEOUT",
            "1s",
            P + "REQUEST_TIMEOUT",
            "2s",
            A + "TOTAL_TIMEOUT",
            "3s",
            P + "MAX_RETRIES",
            "1",
            P + "PROXY",
            "http://proxy:3128",
            P + "USER_AGENT_SUFFIX",
            "app/2");
    OtelCollectorConfig c = OtelCollectorConfig.fromEnv(e);
    assertThat(c.url()).isEqualTo("http://grow:4318");
    assertThat(c.protocol()).isEqualTo(Protocol.HTTP_PROTOBUF);
    assertThat(c.grpcUrl()).isEqualTo("http://grow:4317");
    assertThat(c.signalUrl(Signal.TRACES)).isEqualTo("http://alias:4318/custom/traces");
    assertThat(c.endpointFor(Signal.LOGS)).isEqualTo("http://otel:4318/custom/logs");
    assertThat(c.signalUrl(Signal.METRICS)).isEqualTo("http://grow:4318/v1/metrics");
    assertThat(c.headers())
        .containsEntry("Authorization", "Bearer grow")
        .containsEntry("x-a", "1")
        .containsEntry("x-b", "2")
        .containsEntry("x-c", "3");
    assertThat(c.headersFor(Signal.TRACES)).containsEntry("x-t", "t");
    assertThat(c.headersFor(Signal.LOGS)).doesNotContainKey("x-t");
    assertThat(c.knownSecrets()).contains("Bearer grow", "grow");
    assertThat(c.compression()).isEqualTo(Compression.NONE);
    assertThat(c.exportTimeout()).isEqualTo(Duration.ofMillis(2500));
    assertThat(c.healthUrl()).isEqualTo("http://h:1/");
    assertThat(c.zpagesUrl()).isEqualTo("http://z:2");
    assertThat(c.telemetryUrl()).contains("http://t:8888/metrics");
    assertThat(c.serviceName()).isEqualTo("from-attrs");
    assertThat(c.resourceAttributes())
        .containsEntry("team", "core")
        .containsEntry("region", "us")
        .doesNotContainKey("service.name");
    assertThat(c.serviceVersion()).contains("1.2.3");
    assertThat(c.environment()).contains("staging");
    assertThat(c.serviceResource())
        .containsEntry("service.name", "from-attrs")
        .containsEntry("service.version", "1.2.3")
        .containsEntry("deployment.environment.name", "staging");
    assertThat(c.tracesEnabled()).isTrue();
    assertThat(c.logsEnabled()).isFalse();
    assertThat(c.metricsEnabled()).isFalse();
    assertThat(c.sampler()).isEqualTo(SamplerType.PARENTBASED_TRACEIDRATIO);
    assertThat(c.samplerRatio()).isEqualTo(0.25);
    assertThat(c.propagators()).containsExactly("tracecontext", "b3");
    assertThat(c.metricExportInterval()).isEqualTo(Duration.ofSeconds(5));
    assertThat(c.runtimeMetrics()).isTrue();
    assertThat(c.resourceDetectors()).containsExactly("host", "process");
    assertThat(c.tracesBatch())
        .isEqualTo(new BatchSettings(100, 10, Duration.ofMillis(100), Duration.ofMillis(3000)));
    assertThat(c.logsBatch().maxQueueSize()).isEqualTo(50);
    assertThat(c.logsBatch().maxBatchSize()).isEqualTo(5);
    assertThat(c.logsBatch().flushInterval()).isEqualTo(Duration.ofMillis(200));
    assertThat(c.logsBatch().exportTimeout()).isEqualTo(Duration.ofMillis(4000));
    assertThat(c.tls().caCertPath()).contains(Paths.get("/ca.pem"));
    assertThat(c.connectTimeout()).isEqualTo(Duration.ofSeconds(1));
    assertThat(c.requestTimeout()).isEqualTo(Duration.ofSeconds(2));
    assertThat(c.totalTimeout()).isEqualTo(Duration.ofSeconds(3));
    assertThat(c.retry().maxRetries()).isEqualTo(1);
    assertThat(c.proxy()).contains("http://proxy:3128");
    assertThat(c.userAgentSuffix()).contains("app/2");
    String text = c.toString();
    assertThat(text).doesNotContain("Bearer grow").contains("from-attrs");
  }

  @Test
  void otelEndpointFeedsGrpcUrlInGrpcMode() {
    OtelCollectorConfig c =
        OtelCollectorConfig.fromEnv(
            env(
                "OTEL_EXPORTER_OTLP_PROTOCOL",
                "grpc",
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "http://collector:4317",
                "OTEL_SERVICE_NAME",
                "svc",
                "OTEL_RESOURCE_ATTRIBUTES",
                "service.name=ignored",
                "OTEL_SDK_DISABLED",
                "true",
                A + "SAMPLER_RATIO",
                "0.5",
                P + "TRACES",
                "true",
                A + "RESOURCE_DETECTORS",
                "none",
                A + "CLIENT_CERT",
                "/c.pem",
                "OTEL_EXPORTER_OTLP_CLIENT_KEY",
                "/k.pem",
                P + "INSECURE_SKIP_VERIFY",
                "true"));
    assertThat(c.protocol()).isEqualTo(Protocol.GRPC);
    assertThat(c.grpcUrl()).isEqualTo("http://collector:4317");
    assertThat(c.url()).isEqualTo(OtelCollectorConfig.DEFAULT_URL);
    assertThat(c.endpointFor(Signal.TRACES)).isEqualTo("http://collector:4317");
    assertThat(c.serviceName()).isEqualTo("svc");
    assertThat(c.tracesEnabled()).isTrue();
    assertThat(c.logsEnabled()).isFalse();
    assertThat(c.metricsEnabled()).isFalse();
    assertThat(c.samplerRatio()).isEqualTo(0.5);
    assertThat(c.resourceDetectors()).isEmpty();
    assertThat(c.tls().insecureSkipVerify()).isTrue();
    assertThat(c.tls().clientKeyPath()).contains(Paths.get("/k.pem"));
  }

  @Test
  void explicitValuesWinOverEnvironment() {
    OtelCollectorConfig c =
        OtelCollectorConfig.builder()
            .url("http://explicit:4318")
            .protocol("grpc")
            .header("x-a", "explicit")
            .resourceAttribute("region", "explicit")
            .serviceName("explicit")
            .traces(false)
            .logs(true)
            .metrics(true)
            .sampler(SamplerType.ALWAYS_ON)
            .samplerRatio(0.1)
            .propagators("b3multi")
            .compression(Compression.GZIP)
            .exportTimeout(Duration.ofSeconds(1))
            .metricExportInterval(Duration.ofSeconds(2))
            .runtimeMetrics(false)
            .resourceDetectors(List.of("k8s"))
            .tracesBatch(BatchSettings.defaults())
            .logsBatch(BatchOptions.defaults())
            .signalUrl(Signal.METRICS, "http://m:1/x")
            .signalHeader(Signal.LOGS, "x-l", "1")
            .headers(Map.of("x-b", "b"))
            .resourceAttributes(Map.of("k", "v"))
            .grpcUrl("http://g:4317")
            .healthUrl("http://h:1/")
            .zpagesUrl("http://z:1")
            .telemetryUrl("http://t:1/metrics")
            .serviceVersion("9")
            .environment("prod")
            .fromEnv(
                EnvReader.of(
                    env(
                        P + "URL",
                        "http://env:4318",
                        P + "HEADERS",
                        "x-a=env,x-e=env",
                        P + "RESOURCE_ATTRIBUTES",
                        "region=env",
                        P + "SERVICE_NAME",
                        "env",
                        P + "TRACES",
                        "true",
                        "OTEL_BSP_MAX_QUEUE_SIZE",
                        "5",
                        "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
                        "x-l=env,x-m=env")))
            .build();
    assertThat(c.url()).isEqualTo("http://explicit:4318");
    assertThat(c.protocol()).isEqualTo(Protocol.GRPC);
    assertThat(c.headers()).containsEntry("x-a", "explicit").containsEntry("x-e", "env");
    assertThat(c.resourceAttributes()).containsEntry("region", "explicit");
    assertThat(c.serviceName()).isEqualTo("explicit");
    assertThat(c.tracesEnabled()).isFalse();
    assertThat(c.tracesBatch()).isEqualTo(BatchSettings.defaults());
    assertThat(c.headersFor(Signal.LOGS)).containsEntry("x-l", "1").containsEntry("x-m", "env");
    assertThat(c.endpointFor(Signal.METRICS)).isEqualTo("http://m:1/x");
    assertThat(c.endpointFor(Signal.TRACES)).isEqualTo("http://g:4317");
    OtelCollectorConfig copy = c.toBuilder().build();
    assertThat(copy.toString()).isEqualTo(c.toString());
    assertThat(copy.headersFor(Signal.LOGS)).isEqualTo(c.headersFor(Signal.LOGS));
  }

  @Test
  void toTracingConfig() {
    OtelCollectorConfig c =
        OtelCollectorConfig.builder()
            .url("http://c:4318")
            .serviceName("svc")
            .serviceVersion("1")
            .environment("dev")
            .header("Authorization", "Bearer x")
            .signalHeader(Signal.TRACES, "x-t", "1")
            .tls(TlsOptions.builder().insecureSkipVerify(true).build())
            .build();
    TracingConfig t = c.toTracingConfig();
    assertThat(t.serviceName()).isEqualTo("svc");
    assertThat(t.serviceVersion()).contains("1");
    assertThat(t.environment()).contains("dev");
    assertThat(t.endpoint()).contains("http://c:4318/v1/traces");
    assertThat(t.headers()).containsEntry("x-t", "1").containsEntry("Authorization", "Bearer x");
    assertThat(t.tls()).isPresent();
    assertThat(OtelCollectorConfig.fromEnv(Map.of()).toTracingConfig().tls()).isEmpty();
  }

  @Test
  void invalidEnvironmentValuesNameTheVariable() {
    assertInvalid(env(P + "PROTOCOL", "http/json"), P + "PROTOCOL");
    assertInvalid(env("OTEL_EXPORTER_OTLP_PROTOCOL", "native"), "OTEL_EXPORTER_OTLP_PROTOCOL");
    assertInvalid(env(A + "COMPRESSION", "br"), A + "COMPRESSION");
    assertInvalid(env("OTEL_EXPORTER_OTLP_HEADERS", "novalue"), "OTEL_EXPORTER_OTLP_HEADERS");
    assertInvalid(env(P + "HEADERS", "=x"), P + "HEADERS");
    assertInvalid(env(A + "HEADERS", "a=%zz"), A + "HEADERS");
    assertInvalid(env("OTEL_TRACES_SAMPLER", "jaeger_remote"), "OTEL_TRACES_SAMPLER");
    assertInvalid(env(P + "SAMPLER_RATIO", "2"), P + "SAMPLER_RATIO");
    assertInvalid(env(P + "SAMPLER_RATIO", "x"), P + "SAMPLER_RATIO");
    assertInvalid(env(P + "RESOURCE_DETECTORS", "gcp"), P + "RESOURCE_DETECTORS");
    assertInvalid(env("OTEL_EXPORTER_OTLP_TIMEOUT", "0"), "OTEL_EXPORTER_OTLP_TIMEOUT");
    assertInvalid(env("OTEL_BSP_SCHEDULE_DELAY", "0"), "OTEL_BSP_SCHEDULE_DELAY");
    assertInvalid(env("OTEL_BLRP_MAX_QUEUE_SIZE", "-1"), "OTEL_BLRP_MAX_QUEUE_SIZE");
    assertInvalid(env(P + "MAX_RETRIES", "-1"), P + "MAX_RETRIES");
    assertInvalid(env(P + "TRACES", "maybe"), P + "TRACES");
  }

  private static void assertInvalid(Map<String, String> e, String variable) {
    assertThatThrownBy(() -> OtelCollectorConfig.fromEnv(e))
        .isInstanceOf(ConfigException.class)
        .hasMessageContaining(variable);
  }

  @Test
  void validationRules() {
    assertThatThrownBy(() -> OtelCollectorConfig.builder().protocol(Protocol.NATIVE).build())
        .isInstanceOf(ConfigException.class)
        .hasMessageContaining("no native protocol");
    assertThatThrownBy(() -> OtelCollectorConfig.builder().protocol("native"))
        .isInstanceOf(ConfigException.class);
    assertThatThrownBy(() -> OtelCollectorConfig.builder().url("nope").build())
        .isInstanceOf(ConfigException.class);
    assertThatThrownBy(() -> OtelCollectorConfig.builder().grpcUrl("tcp://x").build())
        .hasMessageContaining("grpcUrl");
    assertThatThrownBy(() -> OtelCollectorConfig.builder().signalUrl(Signal.LOGS, "x").build())
        .hasMessageContaining("logsUrl");
    assertThatThrownBy(() -> OtelCollectorConfig.builder().samplerRatio(1.5).build())
        .hasMessageContaining("samplerRatio");
    assertThatThrownBy(() -> OtelCollectorConfig.builder().propagators(List.of(" ")).build())
        .hasMessageContaining("propagators");
    assertThatThrownBy(
            () -> OtelCollectorConfig.builder().resourceDetectors(List.of("aws")).build())
        .hasMessageContaining("aws");
    assertThatThrownBy(
            () ->
                OtelCollectorConfig.builder()
                    .tracesBatch(
                        new BatchSettings(1, 2, Duration.ofSeconds(1), Duration.ofSeconds(1)))
                    .build())
        .hasMessageContaining("tracesBatch");
    assertThatThrownBy(() -> OtelCollectorConfig.builder().exportTimeout(Duration.ZERO).build())
        .hasMessageContaining("exportTimeout");
    assertThatThrownBy(() -> OtelCollectorConfig.builder().proxy("::bad").build())
        .isInstanceOf(ConfigException.class);
    assertThatThrownBy(() -> OtelCollectorConfig.builder().proxy("relative").build())
        .isInstanceOf(ConfigException.class);
    assertThatThrownBy(
            () ->
                OtelCollectorConfig.builder()
                    .tls(TlsOptions.builder().clientCertPath(Paths.get("/c")).build())
                    .build())
        .isInstanceOf(ConfigException.class);
  }

  @Test
  void parseKeyValueList() {
    assertThat(OtelCollectorConfig.parseKeyValueList("a=1, b = x%2Fy ,,c=d+e", "V"))
        .containsExactly(Map.entry("a", "1"), Map.entry("b", "x/y"), Map.entry("c", "d+e"));
  }
}
