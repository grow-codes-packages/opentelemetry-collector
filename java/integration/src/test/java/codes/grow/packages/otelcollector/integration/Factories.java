package codes.grow.packages.otelcollector.integration;

import codes.grow.packages.otelcollector.OtelCollectorClient;
import codes.grow.packages.otelcollector.OtelCollectorConfig;
import codes.grow.packages.otelcollector.OtelCollectorTracing;
import codes.grow.packages.otelcollector.core.EnvReader;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.testing.ConformanceRunner;
import codes.grow.packages.otelcollector.testing.logging.LoggingTarget;
import codes.grow.packages.otelcollector.testing.tracing.TracingTarget;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import java.time.Duration;

/**
 * Conformance targets of this service (owned by the service repository).
 *
 * <p>{@code core} is the client; {@code tracing} exports OTLP through the collector and reads back
 * from the Jaeger sidecar; {@code logging} is the OTLP log client, read back from the collector's
 * file exporter output. The query adapters live in {@link Readback} (test-only, DECISIONS.md
 * section 7). The collector itself has no query API: {@code tail}, {@code labels}, {@code streams}
 * and {@code dependencies} are not offered.
 */
final class Factories {
  /** Capabilities the deployment under test offers (see the scenario {@code requires}). */
  static final String[] CAPABILITIES = {"version", "query", "services"};

  /** Service name of the tracing scenarios ({@code ${serviceName}}). */
  static final String SERVICE_NAME = "grow-conformance-java";

  private Factories() {}

  static void register(ConformanceRunner.Builder runner, EnvReader env) {
    runner.serviceName(SERVICE_NAME);
    runner.target("core", scenario -> OtelCollectorClient.builder().fromEnv(env).build());
    runner.target(
        "tracing",
        scenario -> {
          OtelCollectorConfig cfg = OtelCollectorConfig.builder().fromEnv(env).build();
          TracingHandle handle =
              OtelCollectorTracing.initTracing(
                  TracingConfig.builder(SERVICE_NAME)
                      .setGlobal(false)
                      .batch(
                          new TracingConfig.BatchSettings(
                              2048, 512, Duration.ofMillis(100), Duration.ofSeconds(30)))
                      .build(),
                  cfg);
          return TracingTarget.of(handle, new Readback.JaegerTraceQuery());
        });
    runner.target(
        "logging",
        scenario -> {
          OtelCollectorClient client = OtelCollectorClient.builder().fromEnv(env).build();
          BatchingLogClient logs =
              client.logClient(LoggingTarget.batchOptions(scenario, client.config().logsBatch()));
          return LoggingTarget.of(logs, new Readback.CaptureLogQuery());
        });
  }
}
