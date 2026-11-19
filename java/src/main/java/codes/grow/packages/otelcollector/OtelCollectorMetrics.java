package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.tracing.Tracing;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.api.metrics.DoubleGauge;
import io.opentelemetry.api.metrics.DoubleHistogram;
import io.opentelemetry.api.metrics.LongCounter;
import io.opentelemetry.api.metrics.LongUpDownCounter;
import io.opentelemetry.api.metrics.Meter;
import io.opentelemetry.exporter.otlp.http.metrics.OtlpHttpMetricExporter;
import io.opentelemetry.exporter.otlp.http.metrics.OtlpHttpMetricExporterBuilder;
import io.opentelemetry.exporter.otlp.metrics.OtlpGrpcMetricExporter;
import io.opentelemetry.exporter.otlp.metrics.OtlpGrpcMetricExporterBuilder;
import io.opentelemetry.sdk.metrics.export.MetricExporter;

/**
 * OTLP metrics for the collector: the official OTLP metric exporter (cumulative temporality) and
 * {@code meter} / {@code counter} / {@code upDownCounter} / {@code histogram} / {@code gauge}
 * helpers on the global {@code MeterProvider} (registered by {@code initTelemetry} unless {@code
 * setGlobal(false)}). A {@link TelemetryHandle} offers the same helpers on its own provider.
 */
public final class OtelCollectorMetrics {
  /** Meter name used by the helpers when none is given. */
  public static final String DEFAULT_METER_NAME = Tracing.INSTRUMENTATION_NAME;

  /** Meter name of the JVM runtime metrics. */
  public static final String RUNTIME_METER_NAME = Tracing.INSTRUMENTATION_NAME + ".runtime";

  private static final Instruments GLOBAL =
      new Instruments(() -> GlobalOpenTelemetry.getOrNoop().getMeterProvider());

  private OtelCollectorMetrics() {}

  /**
   * The official OTLP metric exporter for a collector config: {@code http/protobuf} → {@code
   * OtlpHttpMetricExporter} ({@code metricsUrl}), {@code grpc} → {@code OtlpGrpcMetricExporter}.
   *
   * @param config collector config
   * @return the exporter
   */
  public static MetricExporter metricExporter(OtelCollectorConfig config) {
    String url = config.endpointFor(Signal.METRICS);
    String compression =
        config.compression() == Compression.GZIP ? "gzip" : Compression.NONE.wireValue();
    if (config.protocol() == Protocol.GRPC) {
      OtlpGrpcMetricExporterBuilder b =
          OtlpGrpcMetricExporter.builder()
              .setEndpoint(url)
              .setCompression(compression)
              .setTimeout(config.exportTimeout())
              .setConnectTimeout(config.connectTimeout());
      config.headersFor(Signal.METRICS).forEach(b::addHeader);
      Otlp.exporterRetryPolicy(config.retry())
          .ifPresentOrElse(b::setRetryPolicy, () -> b.setRetryPolicy(null));
      Otlp.applyTls(config.tls(), b::setSslContext, b::setTrustedCertificates, b::setClientTls);
      return b.build();
    }
    OtlpHttpMetricExporterBuilder b =
        OtlpHttpMetricExporter.builder()
            .setEndpoint(url)
            .setCompression(compression)
            .setTimeout(config.exportTimeout())
            .setConnectTimeout(config.connectTimeout());
    config.headersFor(Signal.METRICS).forEach(b::addHeader);
    Otlp.exporterRetryPolicy(config.retry())
        .ifPresentOrElse(b::setRetryPolicy, () -> b.setRetryPolicy(null));
    Otlp.applyTls(config.tls(), b::setSslContext, b::setTrustedCertificates, b::setClientTls);
    return b.build();
  }

  /**
   * A meter named {@link #DEFAULT_METER_NAME} from the global provider.
   *
   * @return the meter
   */
  public static Meter meter() {
    return GLOBAL.meter(null, null);
  }

  /**
   * A meter from the global provider.
   *
   * @param name instrumentation scope name
   * @return the meter
   */
  public static Meter meter(String name) {
    return GLOBAL.meter(name, null);
  }

  /**
   * Monotonic counter (OTLP {@code sum}, {@code isMonotonic}, cumulative) on the global provider.
   *
   * @param name instrument name
   * @param options description, unit, meter
   * @return the counter
   */
  public static LongCounter counter(String name, InstrumentOptions options) {
    return GLOBAL.counter(name, options);
  }

  /**
   * Monotonic counter without options.
   *
   * @param name instrument name
   * @return the counter
   */
  public static LongCounter counter(String name) {
    return counter(name, InstrumentOptions.NONE);
  }

  /**
   * Up/down counter (OTLP non-monotonic {@code sum}) on the global provider.
   *
   * @param name instrument name
   * @param options description, unit, meter
   * @return the counter
   */
  public static LongUpDownCounter upDownCounter(String name, InstrumentOptions options) {
    return GLOBAL.upDownCounter(name, options);
  }

  /**
   * Histogram with optional explicit bucket boundaries on the global provider.
   *
   * @param name instrument name
   * @param options description, unit, meter, boundaries
   * @return the histogram
   */
  public static DoubleHistogram histogram(String name, InstrumentOptions options) {
    return GLOBAL.histogram(name, options);
  }

  /**
   * Synchronous gauge (OTLP {@code gauge}, last value) on the global provider.
   *
   * @param name instrument name
   * @param options description, unit, meter
   * @return the gauge
   */
  public static DoubleGauge gauge(String name, InstrumentOptions options) {
    return GLOBAL.gauge(name, options);
  }
}
