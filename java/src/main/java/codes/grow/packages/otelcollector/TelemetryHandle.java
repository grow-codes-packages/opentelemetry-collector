package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.OtelCollectorException;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.UnavailableException;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.LogClient;
import codes.grow.packages.otelcollector.logging.Logger;
import codes.grow.packages.otelcollector.logging.Severity;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import io.opentelemetry.api.OpenTelemetry;
import io.opentelemetry.api.logs.LoggerProvider;
import io.opentelemetry.api.metrics.DoubleGauge;
import io.opentelemetry.api.metrics.DoubleHistogram;
import io.opentelemetry.api.metrics.LongCounter;
import io.opentelemetry.api.metrics.LongUpDownCounter;
import io.opentelemetry.api.metrics.Meter;
import io.opentelemetry.api.metrics.MeterProvider;
import io.opentelemetry.api.trace.Tracer;
import io.opentelemetry.api.trace.TracerProvider;
import io.opentelemetry.sdk.common.CompletableResultCode;
import io.opentelemetry.sdk.logs.SdkLoggerProvider;
import io.opentelemetry.sdk.metrics.SdkMeterProvider;
import io.opentelemetry.sdk.resources.Resource;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * A running telemetry setup (all three signals) created by {@link
 * OtelCollectorTelemetry#initTelemetry}. Disabled signals hand out no-op tracers, meters and
 * loggers. {@link #shutdown()} flushes and stops every provider and is idempotent.
 */
public final class TelemetryHandle implements AutoCloseable {
  /** Default deadline of {@link #forceFlush()}. */
  public static final Duration DEFAULT_FLUSH_TIMEOUT = Duration.ofSeconds(30);

  private final OtelCollectorConfig config;
  private final Resource resource;
  private final Map<String, Object> resourceAttributes;
  private final TracingHandle tracing;
  private final SdkMeterProvider meterProvider;
  private final SdkLoggerProvider loggerProvider;
  private final BatchingLogClient logClient;
  private final RuntimeMetrics runtime;
  private final OpenTelemetry openTelemetry;
  private final Instruments instruments;
  private final AtomicBoolean shutdown = new AtomicBoolean();

  TelemetryHandle(
      OtelCollectorConfig config,
      Resource resource,
      Map<String, Object> resourceAttributes,
      TracingHandle tracing,
      SdkMeterProvider meterProvider,
      SdkLoggerProvider loggerProvider,
      BatchingLogClient logClient,
      RuntimeMetrics runtime,
      OpenTelemetry openTelemetry) {
    this.config = config;
    this.resource = resource;
    this.resourceAttributes = Map.copyOf(resourceAttributes);
    this.tracing = tracing;
    this.meterProvider = meterProvider;
    this.loggerProvider = loggerProvider;
    this.logClient = logClient;
    this.runtime = runtime;
    this.openTelemetry = openTelemetry;
    this.instruments = new Instruments(this::meterProvider);
  }

  /**
   * Returns the collector config in use.
   *
   * @return the config
   */
  public OtelCollectorConfig config() {
    return config;
  }

  /**
   * Returns the shared resource (SDK defaults plus detected, configured and service attributes).
   *
   * @return the resource
   */
  public Resource resource() {
    return resource;
  }

  /**
   * Returns the resource attributes added to the SDK defaults (detected &lt; configured &lt;
   * service fields).
   *
   * @return immutable map
   */
  public Map<String, Object> resourceAttributes() {
    return resourceAttributes;
  }

  /**
   * Returns the combined OpenTelemetry instance (tracer, meter and logger providers, propagators).
   *
   * @return the instance
   */
  public OpenTelemetry openTelemetry() {
    return openTelemetry;
  }

  /**
   * Returns the tracing handle.
   *
   * @return the handle, empty when traces are disabled
   */
  public Optional<TracingHandle> tracing() {
    return Optional.ofNullable(tracing);
  }

  /**
   * Returns the tracer provider (no-op when traces are disabled).
   *
   * @return the provider
   */
  public TracerProvider tracerProvider() {
    return tracing == null ? TracerProvider.noop() : tracing.tracerProvider();
  }

  /**
   * A tracer named after the service.
   *
   * @return the tracer
   */
  public Tracer tracer() {
    return tracer(config.serviceName());
  }

  /**
   * A tracer.
   *
   * @param name instrumentation scope name
   * @return the tracer
   */
  public Tracer tracer(String name) {
    return tracerProvider().get(name);
  }

  /**
   * A tracer with a scope version.
   *
   * @param name instrumentation scope name
   * @param version scope version
   * @return the tracer
   */
  public Tracer tracer(String name, String version) {
    return tracerProvider().get(name, version);
  }

  /**
   * Returns the meter provider (no-op when metrics are disabled).
   *
   * @return the provider
   */
  public MeterProvider meterProvider() {
    return meterProvider == null ? MeterProvider.noop() : meterProvider;
  }

  /**
   * A meter named {@link OtelCollectorMetrics#DEFAULT_METER_NAME}.
   *
   * @return the meter
   */
  public Meter meter() {
    return instruments.meter(null, null);
  }

  /**
   * A meter.
   *
   * @param name instrumentation scope name
   * @return the meter
   */
  public Meter meter(String name) {
    return instruments.meter(name, null);
  }

  /**
   * Monotonic counter (OTLP {@code sum}, cumulative).
   *
   * @param name instrument name
   * @param options description, unit, meter
   * @return the counter
   */
  public LongCounter counter(String name, InstrumentOptions options) {
    return instruments.counter(name, options);
  }

  /**
   * Up/down counter (OTLP non-monotonic {@code sum}).
   *
   * @param name instrument name
   * @param options description, unit, meter
   * @return the counter
   */
  public LongUpDownCounter upDownCounter(String name, InstrumentOptions options) {
    return instruments.upDownCounter(name, options);
  }

  /**
   * Histogram with optional explicit bucket boundaries.
   *
   * @param name instrument name
   * @param options description, unit, meter, boundaries
   * @return the histogram
   */
  public DoubleHistogram histogram(String name, InstrumentOptions options) {
    return instruments.histogram(name, options);
  }

  /**
   * Synchronous gauge (OTLP {@code gauge}).
   *
   * @param name instrument name
   * @param options description, unit, meter
   * @return the gauge
   */
  public DoubleGauge gauge(String name, InstrumentOptions options) {
    return instruments.gauge(name, options);
  }

  /**
   * Returns the OpenTelemetry logger provider for log appenders (no-op when logs are disabled).
   *
   * @return the provider
   */
  public LoggerProvider loggerProvider() {
    return loggerProvider == null ? LoggerProvider.noop() : loggerProvider;
  }

  /**
   * Returns the OTLP log client (SPEC section 4).
   *
   * @return the client, empty when logs are disabled
   */
  public Optional<LogClient> logClient() {
    return Optional.ofNullable(logClient);
  }

  /**
   * A logger writing to {@link #logClient()} (drops records when logs are disabled).
   *
   * @param name logger name
   * @return the logger
   */
  public Logger logger(String name) {
    return logger(name, Map.of());
  }

  /**
   * A logger with fixed attributes.
   *
   * @param name logger name
   * @param attributes attributes added to every record
   * @return the logger
   */
  public Logger logger(String name, Map<String, ?> attributes) {
    if (logClient != null) {
      return logClient.logger(name, attributes);
    }
    return new Logger(NoopLogClient.INSTANCE, name, attributes, Severity.INFO);
  }

  /**
   * Exports everything buffered in every enabled signal, waiting at most {@link
   * #DEFAULT_FLUSH_TIMEOUT}.
   *
   * @throws TimeoutException when the flush does not complete in time
   */
  public void forceFlush() {
    forceFlush(DEFAULT_FLUSH_TIMEOUT);
  }

  /**
   * Exports everything buffered in every enabled signal.
   *
   * @param timeout deadline
   * @throws TimeoutException when the flush does not complete in time
   * @throws UnavailableException when an exporter failed
   */
  public void forceFlush(Duration timeout) {
    long deadline = System.nanoTime() + timeout.toNanos();
    List<CompletableResultCode> codes = new ArrayList<>();
    if (tracing != null) {
      codes.add(tracing.tracerProvider().forceFlush());
    }
    if (meterProvider != null) {
      codes.add(meterProvider.forceFlush());
    }
    if (loggerProvider != null) {
      codes.add(loggerProvider.forceFlush());
    }
    if (logClient != null) {
      logClient.flush(timeout);
    }
    await(codes, deadline, timeout, "telemetry.forceFlush");
  }

  /**
   * Asynchronous {@link #forceFlush(Duration)}.
   *
   * @param timeout deadline
   * @return a future completed when flushed
   */
  public CompletableFuture<Void> forceFlushAsync(Duration timeout) {
    return CompletableFuture.runAsync(() -> forceFlush(timeout));
  }

  private static void await(
      List<CompletableResultCode> codes, long deadline, Duration timeout, String op) {
    CompletableResultCode all = CompletableResultCode.ofAll(codes);
    all.join(Math.max(0, deadline - System.nanoTime()), TimeUnit.NANOSECONDS);
    if (!all.isDone()) {
      throw new TimeoutException(op + ": not completed within " + timeout.toMillis() + " ms");
    }
    if (!all.isSuccess()) {
      throw new UnavailableException(op + ": an exporter failed");
    }
  }

  /**
   * Flushes and shuts every provider down (log client, logger, meter and tracer providers).
   * Idempotent. A global registration stays in place (OpenTelemetry allows one per JVM).
   *
   * @throws OtelCollectorException when a provider fails to shut down in time
   */
  public void shutdown() {
    if (!shutdown.compareAndSet(false, true)) {
      return;
    }
    Duration timeout = config.exportTimeout().plusSeconds(5);
    long deadline = System.nanoTime() + timeout.toNanos();
    OtelCollectorException first = null;
    if (runtime != null) {
      runtime.close();
    }
    if (logClient != null) {
      try {
        logClient.close();
      } catch (OtelCollectorException e) {
        first = e;
      }
    }
    List<CompletableResultCode> codes = new ArrayList<>();
    if (loggerProvider != null) {
      codes.add(loggerProvider.shutdown());
    }
    if (meterProvider != null) {
      codes.add(meterProvider.shutdown());
    }
    if (tracing != null) {
      try {
        tracing.shutdown();
      } catch (OtelCollectorException e) {
        first = first == null ? e : first;
      }
    }
    try {
      await(codes, deadline, timeout, "telemetry.shutdown");
    } catch (OtelCollectorException e) {
      first = first == null ? e : first;
    }
    if (first != null) {
      throw first;
    }
  }

  void shutdownQuietly() {
    try {
      shutdown();
    } catch (OtelCollectorException ignored) {
      // best effort after a failed start
    }
  }

  /**
   * Asynchronous {@link #shutdown()}.
   *
   * @return a future completed when shut down
   */
  public CompletableFuture<Void> shutdownAsync() {
    return CompletableFuture.runAsync(this::shutdown);
  }

  /**
   * Returns whether {@link #shutdown()} was called.
   *
   * @return true once shut down
   */
  public boolean isShutdown() {
    return shutdown.get();
  }

  /** Same as {@link #shutdown()}. */
  @Override
  public void close() {
    shutdown();
  }
}
