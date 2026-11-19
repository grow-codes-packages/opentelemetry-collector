package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.logging.BatchOptions;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.LogExporter;
import codes.grow.packages.otelcollector.tracing.SpanExporterFactory;
import codes.grow.packages.otelcollector.tracing.Tracing;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.sdk.OpenTelemetrySdk;
import io.opentelemetry.sdk.OpenTelemetrySdkBuilder;
import io.opentelemetry.sdk.logs.SdkLoggerProvider;
import io.opentelemetry.sdk.logs.export.BatchLogRecordProcessor;
import io.opentelemetry.sdk.logs.export.LogRecordExporter;
import io.opentelemetry.sdk.metrics.SdkMeterProvider;
import io.opentelemetry.sdk.metrics.export.MetricExporter;
import io.opentelemetry.sdk.metrics.export.PeriodicMetricReader;
import io.opentelemetry.sdk.resources.Resource;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * One-call OpenTelemetry bootstrap for every Grow stack (the platform deploys the collector with
 * each stack): traces ({@link OtelCollectorTracing}), logs (an OpenTelemetry {@code LoggerProvider}
 * for log appenders plus a {@link BatchingLogClient}) and metrics ({@code MeterProvider} with a
 * periodic reader) with one shared resource and the official OTLP exporters.
 */
public final class OtelCollectorTelemetry {
  private OtelCollectorTelemetry() {}

  /**
   * Starts telemetry from the environment ({@link OtelCollectorConfig#fromEnv()}).
   *
   * @return the handle
   */
  public static TelemetryHandle initTelemetry() {
    return initTelemetry(OtelCollectorConfig.fromEnv());
  }

  /**
   * Starts telemetry for a config with the default options.
   *
   * @param config collector config
   * @return the handle
   */
  public static TelemetryHandle initTelemetry(OtelCollectorConfig config) {
    return initTelemetry(config, TelemetryOptions.defaults());
  }

  /**
   * Resource attributes for a config: detected &lt; configured attributes &lt; service fields
   * ({@code service.name}, {@code service.version}, {@code deployment.environment.name} and the
   * older {@code deployment.environment}).
   *
   * @param config collector config
   * @param detector resource detector
   * @return a new map
   */
  public static Map<String, Object> resourceAttributes(
      OtelCollectorConfig config, ResourceDetector detector) {
    Map<String, Object> attrs = new LinkedHashMap<>(detector.detect(config.resourceDetectors()));
    attrs.putAll(config.serviceResource());
    config.environment().ifPresent(e -> attrs.put("deployment.environment", e));
    return attrs;
  }

  /**
   * Starts telemetry. Signals follow the config ({@code traces} / {@code logs} / {@code metrics},
   * {@code OTEL_SDK_DISABLED}, {@code OTEL_<SIGNAL>_EXPORTER=none}) unless the options turn them on
   * or off. Exporter failures never throw at the caller.
   *
   * @param config collector config
   * @param options options
   * @return the handle
   * @throws ConfigException for invalid settings or when a global OpenTelemetry already exists
   */
  public static TelemetryHandle initTelemetry(
      OtelCollectorConfig config, TelemetryOptions options) {
    config.validate();
    Map<String, Object> attrs = resourceAttributes(config, options.resourceDetector());
    Resource resource = Resource.getDefault().merge(Resource.create(OtelValues.attributes(attrs)));

    TracingHandle tracing = null;
    SdkMeterProvider meterProvider = null;
    SdkLoggerProvider loggerProvider = null;
    BatchingLogClient logClient = null;
    RuntimeMetrics runtime = null;
    try {
      if (options.traces(config)) {
        TracingConfig tc =
            config.toTracingConfig().toBuilder().resourceAttributes(attrs).setGlobal(false).build();
        SpanExporterFactory factory =
            options
                .spanExporterFactory()
                .orElse(c -> OtelCollectorTracing.createSpanExporter(c, config.retry()));
        tracing = Tracing.initWith(tc, factory);
      }
      if (options.metrics(config)) {
        MetricExporter exporter =
            options.metricExporter().orElseGet(() -> OtelCollectorMetrics.metricExporter(config));
        meterProvider =
            SdkMeterProvider.builder()
                .setResource(resource)
                .registerMetricReader(
                    PeriodicMetricReader.builder(exporter)
                        .setInterval(options.metricExportInterval(config))
                        .build())
                .build();
        if (options.runtimeMetrics(config)) {
          runtime =
              RuntimeMetrics.register(meterProvider.get(OtelCollectorMetrics.RUNTIME_METER_NAME));
        }
      }
      if (options.logs(config)) {
        LogRecordExporter exporter =
            options
                .logRecordExporter()
                .orElseGet(() -> OtlpLogExporter.officialExporter(config, true));
        BatchOptions lb = config.logsBatch();
        loggerProvider =
            SdkLoggerProvider.builder()
                .setResource(resource)
                .addLogRecordProcessor(
                    BatchLogRecordProcessor.builder(exporter)
                        .setMaxQueueSize(lb.maxQueueSize())
                        .setMaxExportBatchSize(Math.min(lb.maxBatchSize(), lb.maxQueueSize()))
                        .setScheduleDelay(lb.flushInterval())
                        .setExporterTimeout(lb.exportTimeout())
                        .build())
                .build();
        LogExporter clientExporter =
            options.logExporter().orElseGet(() -> new OtlpLogExporter(config));
        logClient =
            OtelCollectorLogs.logClientBuilder(config, clientExporter).resource(attrs).build();
      }
      OpenTelemetrySdkBuilder sdk =
          OpenTelemetrySdk.builder()
              .setPropagators(
                  tracing != null
                      ? tracing.propagators()
                      : Tracing.propagators(config.propagators(), Map.of()));
      if (tracing != null) {
        sdk.setTracerProvider(tracing.tracerProvider());
      }
      if (meterProvider != null) {
        sdk.setMeterProvider(meterProvider);
      }
      if (loggerProvider != null) {
        sdk.setLoggerProvider(loggerProvider);
      }
      OpenTelemetrySdk openTelemetry = sdk.build();
      if (options.setGlobal()) {
        try {
          GlobalOpenTelemetry.set(openTelemetry);
        } catch (IllegalStateException e) {
          throw new ConfigException(
              "telemetry: a global OpenTelemetry is already registered; use setGlobal(false)", e);
        }
      }
      return new TelemetryHandle(
          config,
          resource,
          attrs,
          tracing,
          meterProvider,
          loggerProvider,
          logClient,
          runtime,
          openTelemetry);
    } catch (RuntimeException e) {
      new TelemetryHandle(
              config,
              resource,
              attrs,
              tracing,
              meterProvider,
              loggerProvider,
              logClient,
              runtime,
              null)
          .shutdownQuietly();
      throw e;
    }
  }
}
