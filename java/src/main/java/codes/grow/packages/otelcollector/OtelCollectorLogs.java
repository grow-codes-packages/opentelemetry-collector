package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.logging.BatchOptions;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.LogExporter;

/**
 * OTLP log clients for the collector: a {@link BatchingLogClient} (SPEC section 4) over {@link
 * OtlpLogExporter}, with the service resource ({@code service.name}, version, environment and the
 * configured resource attributes) and the configured log batch settings ({@code OTEL_BLRP_*}).
 */
public final class OtelCollectorLogs {
  private OtelCollectorLogs() {}

  /**
   * A log client builder preset with an {@link OtlpLogExporter}, the service resource and the
   * configured batch options; customise redaction, resource or batching before {@code build()}.
   *
   * @param config collector config
   * @return the builder
   */
  public static BatchingLogClient.Builder logClientBuilder(OtelCollectorConfig config) {
    return logClientBuilder(config, new OtlpLogExporter(config));
  }

  /**
   * A log client builder preset like {@link #logClientBuilder(OtelCollectorConfig)} with another
   * exporter (tests).
   *
   * @param config collector config
   * @param exporter exporter
   * @return the builder
   */
  public static BatchingLogClient.Builder logClientBuilder(
      OtelCollectorConfig config, LogExporter exporter) {
    return BatchingLogClient.builder(exporter)
        .resource(config.serviceResource())
        .batchOptions(config.logsBatch());
  }

  /**
   * A log client exporting OTLP logs to the collector.
   *
   * @param config collector config
   * @return the client (close it to flush)
   */
  public static BatchingLogClient createLogClient(OtelCollectorConfig config) {
    return logClientBuilder(config).build();
  }

  /**
   * A log client exporting OTLP logs to the collector with explicit batch options.
   *
   * @param config collector config
   * @param batch batch options
   * @return the client (close it to flush)
   */
  public static BatchingLogClient createLogClient(OtelCollectorConfig config, BatchOptions batch) {
    return logClientBuilder(config).batchOptions(batch).build();
  }
}
