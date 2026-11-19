// Created by sdk-kit 0.1.0; owned by this repository.
package codes.grow.packages.otelcollector.examples;

import codes.grow.packages.otelcollector.InstrumentOptions;
import codes.grow.packages.otelcollector.OtelCollectorClient;
import codes.grow.packages.otelcollector.TelemetryHandle;
import codes.grow.packages.otelcollector.core.AuthenticationException;
import codes.grow.packages.otelcollector.core.HealthStatus;
import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.logging.Logger;
import io.opentelemetry.api.metrics.DoubleHistogram;
import io.opentelemetry.api.metrics.LongCounter;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.context.Scope;
import java.time.Duration;
import java.util.Map;

/**
 * Sends traces, metrics and logs to an OpenTelemetry Collector: configuration from the
 * environment, health, ping, server version, the one-call telemetry bootstrap and the log client.
 */
public final class BasicExample {
  private BasicExample() {}

  /**
   * Runs the example.
   *
   * @param args unused
   */
  public static void main(String[] args) {
    // Reads GROW_OPENTELEMETRY_COLLECTOR_*, GROW_OTEL_* and the standard OTEL_* variables.
    try (var client = OtelCollectorClient.fromEnv()) {
      System.out.println("config: " + client.config()); // header values print as [REDACTED]
      HealthStatus health = client.waitUntilReady(Duration.ofSeconds(10));
      System.out.println("health: " + health.status() + " via " + health.details().get("source"));
      try {
        System.out.println("ping: " + client.ping().toMillis() + " ms");
      } catch (AuthenticationException e) {
        System.out.println("the collector rejected the configured headers: " + e.getMessage());
        return;
      }
      try {
        System.out.println("collector version: " + client.serverVersion());
      } catch (UnsupportedException e) {
        System.out.println("the collector exposes no version");
      }

      // One call for traces, metrics and logs (OTLP, one shared resource); closed with the client.
      TelemetryHandle telemetry = client.initTelemetry();
      LongCounter requests = telemetry.counter("example_requests", InstrumentOptions.unit("1"));
      DoubleHistogram latency =
          telemetry.histogram("example_latency", InstrumentOptions.boundaries(10.0, 100.0, 1000.0));
      Logger logger = telemetry.logger("example");

      Span span = telemetry.tracer("example").spanBuilder("handle-request").startSpan();
      try (Scope scope = span.makeCurrent()) {
        requests.add(1);
        latency.record(42.0);
        span.addEvent("work-done");
        logger.info("request handled", Map.of("items", 3)); // carries trace_id / span_id
      } finally {
        span.end();
      }
      telemetry.forceFlush(Duration.ofSeconds(10));
      System.out.println("trace id: " + span.getSpanContext().getTraceId());
    }
  }
}
