package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.logging.LogExporter;
import codes.grow.packages.otelcollector.tracing.SpanExporterFactory;
import io.opentelemetry.sdk.logs.export.LogRecordExporter;
import io.opentelemetry.sdk.metrics.export.MetricExporter;
import java.time.Duration;
import java.util.Optional;

/**
 * Options of {@link OtelCollectorTelemetry#initTelemetry(OtelCollectorConfig, TelemetryOptions)}:
 * per-signal switches over the config, global registration, metric settings and exporter
 * replacements (tests).
 */
public final class TelemetryOptions {
  private final Boolean traces;
  private final Boolean logs;
  private final Boolean metrics;
  private final boolean setGlobal;
  private final Duration metricExportInterval;
  private final Boolean runtimeMetrics;
  private final SpanExporterFactory spanExporterFactory;
  private final MetricExporter metricExporter;
  private final LogRecordExporter logRecordExporter;
  private final LogExporter logExporter;
  private final ResourceDetector resourceDetector;

  private TelemetryOptions(Builder b) {
    this.traces = b.traces;
    this.logs = b.logs;
    this.metrics = b.metrics;
    this.setGlobal = b.setGlobal;
    this.metricExportInterval = b.metricExportInterval;
    this.runtimeMetrics = b.runtimeMetrics;
    this.spanExporterFactory = b.spanExporterFactory;
    this.metricExporter = b.metricExporter;
    this.logRecordExporter = b.logRecordExporter;
    this.logExporter = b.logExporter;
    this.resourceDetector = b.resourceDetector;
  }

  /**
   * The defaults: signals from the config, global registration on.
   *
   * @return options
   */
  public static TelemetryOptions defaults() {
    return builder().build();
  }

  /**
   * Starts a builder.
   *
   * @return a new builder
   */
  public static Builder builder() {
    return new Builder();
  }

  boolean traces(OtelCollectorConfig c) {
    return traces != null ? traces : c.tracesEnabled();
  }

  boolean logs(OtelCollectorConfig c) {
    return logs != null ? logs : c.logsEnabled();
  }

  boolean metrics(OtelCollectorConfig c) {
    return metrics != null ? metrics : c.metricsEnabled();
  }

  boolean runtimeMetrics(OtelCollectorConfig c) {
    return runtimeMetrics != null ? runtimeMetrics : c.runtimeMetrics();
  }

  Duration metricExportInterval(OtelCollectorConfig c) {
    return metricExportInterval != null ? metricExportInterval : c.metricExportInterval();
  }

  /**
   * Returns whether the providers are registered as the global OpenTelemetry.
   *
   * @return true by default
   */
  public boolean setGlobal() {
    return setGlobal;
  }

  Optional<SpanExporterFactory> spanExporterFactory() {
    return Optional.ofNullable(spanExporterFactory);
  }

  Optional<MetricExporter> metricExporter() {
    return Optional.ofNullable(metricExporter);
  }

  Optional<LogRecordExporter> logRecordExporter() {
    return Optional.ofNullable(logRecordExporter);
  }

  Optional<LogExporter> logExporter() {
    return Optional.ofNullable(logExporter);
  }

  ResourceDetector resourceDetector() {
    return resourceDetector != null ? resourceDetector : ResourceDetector.system();
  }

  /** Builder of {@link TelemetryOptions}. */
  public static final class Builder {
    private Boolean traces;
    private Boolean logs;
    private Boolean metrics;
    private boolean setGlobal = true;
    private Duration metricExportInterval;
    private Boolean runtimeMetrics;
    private SpanExporterFactory spanExporterFactory;
    private MetricExporter metricExporter;
    private LogRecordExporter logRecordExporter;
    private LogExporter logExporter;
    private ResourceDetector resourceDetector;

    private Builder() {}

    /**
     * Turns traces on or off (default: config {@code tracesEnabled}).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder traces(boolean enabled) {
      this.traces = enabled;
      return this;
    }

    /**
     * Turns logs on or off (default: config {@code logsEnabled}).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder logs(boolean enabled) {
      this.logs = enabled;
      return this;
    }

    /**
     * Turns metrics on or off (default: config {@code metricsEnabled}).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder metrics(boolean enabled) {
      this.metrics = enabled;
      return this;
    }

    /**
     * Registers the providers as the global OpenTelemetry (default true; only one registration per
     * JVM is possible).
     *
     * @param setGlobal register
     * @return this builder
     */
    public Builder setGlobal(boolean setGlobal) {
      this.setGlobal = setGlobal;
      return this;
    }

    /**
     * Overrides the metric export interval.
     *
     * @param interval positive duration
     * @return this builder
     */
    public Builder metricExportInterval(Duration interval) {
      this.metricExportInterval = interval;
      return this;
    }

    /**
     * Overrides the runtime metrics switch.
     *
     * @param enabled on
     * @return this builder
     */
    public Builder runtimeMetrics(boolean enabled) {
      this.runtimeMetrics = enabled;
      return this;
    }

    /**
     * Replaces the OTLP span exporter factory.
     *
     * @param factory factory
     * @return this builder
     */
    public Builder spanExporterFactory(SpanExporterFactory factory) {
      this.spanExporterFactory = factory;
      return this;
    }

    /**
     * Replaces the OTLP metric exporter.
     *
     * @param exporter exporter
     * @return this builder
     */
    public Builder metricExporter(MetricExporter exporter) {
      this.metricExporter = exporter;
      return this;
    }

    /**
     * Replaces the OTLP log record exporter of the {@code LoggerProvider}.
     *
     * @param exporter exporter
     * @return this builder
     */
    public Builder logRecordExporter(LogRecordExporter exporter) {
      this.logRecordExporter = exporter;
      return this;
    }

    /**
     * Replaces the exporter of the {@link codes.grow.packages.otelcollector.logging.LogClient}.
     *
     * @param exporter exporter
     * @return this builder
     */
    public Builder logExporter(LogExporter exporter) {
      this.logExporter = exporter;
      return this;
    }

    /**
     * Replaces the resource detector (tests).
     *
     * @param detector detector
     * @return this builder
     */
    public Builder resourceDetector(ResourceDetector detector) {
      this.resourceDetector = detector;
      return this;
    }

    /**
     * Builds the options.
     *
     * @return options
     */
    public TelemetryOptions build() {
      return new TelemetryOptions(this);
    }
  }
}
