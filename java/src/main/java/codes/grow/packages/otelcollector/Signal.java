package codes.grow.packages.otelcollector;

import java.util.Locale;

/** The three OpenTelemetry signals the collector receives. */
public enum Signal {
  /** Spans ({@code /v1/traces}, {@code TraceService/Export}). */
  TRACES,
  /** Log records ({@code /v1/logs}, {@code LogsService/Export}). */
  LOGS,
  /** Metric points ({@code /v1/metrics}, {@code MetricsService/Export}). */
  METRICS;

  /**
   * Lower-case name used in URLs and variable names: {@code traces}, {@code logs}, {@code metrics}.
   *
   * @return the path segment
   */
  public String path() {
    return name().toLowerCase(Locale.ROOT);
  }
}
