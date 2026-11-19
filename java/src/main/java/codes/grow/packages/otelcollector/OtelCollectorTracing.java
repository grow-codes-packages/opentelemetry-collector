package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.tracing.SpanExporterFactory;
import codes.grow.packages.otelcollector.tracing.Tracing;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.BatchSettings;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingConfig.SamplerType;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import io.opentelemetry.exporter.otlp.http.trace.OtlpHttpSpanExporter;
import io.opentelemetry.exporter.otlp.http.trace.OtlpHttpSpanExporterBuilder;
import io.opentelemetry.exporter.otlp.trace.OtlpGrpcSpanExporter;
import io.opentelemetry.exporter.otlp.trace.OtlpGrpcSpanExporterBuilder;
import io.opentelemetry.sdk.trace.export.SpanExporter;
import java.net.URI;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Tracing preset for the OpenTelemetry Collector (SPEC section 5): the official OTLP span exporters
 * ({@code http/protobuf} to {@code http://127.0.0.1:4318/v1/traces} by default, or {@code grpc} to
 * {@code http://127.0.0.1:4317}) and {@code initTracing} with collector defaults from the
 * environment.
 */
public final class OtelCollectorTracing {
  private OtelCollectorTracing() {}

  /**
   * The traces exporter URL: for {@code http/protobuf} an endpoint without a path (a base such as
   * {@code http://collector:4318}) gets {@code /v1/traces}; anything else is used as is.
   *
   * @param protocol protocol
   * @param endpoint endpoint
   * @return the URL
   */
  public static String resolveTracesEndpoint(Protocol protocol, String endpoint) {
    if (protocol == Protocol.GRPC) {
      return endpoint;
    }
    String path = URI.create(endpoint).getPath();
    if (path == null || path.isEmpty() || "/".equals(path)) {
      return endpoint.replaceAll("/+$", "") + "/v1/traces";
    }
    return endpoint;
  }

  /**
   * The span exporter factory of this preset (see {@link #createSpanExporter}).
   *
   * @return factory for {@link Tracing#initWith}
   */
  public static SpanExporterFactory spanExporterFactory() {
    return OtelCollectorTracing::createSpanExporter;
  }

  /**
   * Builds the official OTLP span exporter for a tracing config: {@code http/protobuf} → {@code
   * OtlpHttpSpanExporter}, {@code grpc} → {@code OtlpGrpcSpanExporter}. The export deadline is
   * {@code batch.exportTimeout}; retries follow the OTLP rules of the official exporter.
   *
   * @param config tracing config
   * @return exporter
   * @throws ConfigException for the {@code native} protocol or unreadable TLS files
   */
  public static SpanExporter createSpanExporter(TracingConfig config) {
    return createSpanExporter(config, RetryPolicy.defaults());
  }

  static SpanExporter createSpanExporter(TracingConfig config, RetryPolicy retry) {
    TlsOptions tls = config.tls().orElse(TlsOptions.defaults());
    switch (config.protocol()) {
      case GRPC:
        {
          OtlpGrpcSpanExporterBuilder b =
              OtlpGrpcSpanExporter.builder()
                  .setEndpoint(config.endpoint().orElse(OtelCollectorConfig.DEFAULT_GRPC_URL))
                  .setCompression(config.compression().wireValue())
                  .setTimeout(config.batch().exportTimeout());
          config.headers().forEach(b::addHeader);
          Otlp.exporterRetryPolicy(retry)
              .ifPresentOrElse(b::setRetryPolicy, () -> b.setRetryPolicy(null));
          Otlp.applyTls(tls, b::setSslContext, b::setTrustedCertificates, b::setClientTls);
          return b.build();
        }
      case HTTP_PROTOBUF:
        {
          String endpoint =
              resolveTracesEndpoint(
                  Protocol.HTTP_PROTOBUF,
                  config.endpoint().orElse(OtelCollectorConfig.DEFAULT_URL));
          OtlpHttpSpanExporterBuilder b =
              OtlpHttpSpanExporter.builder()
                  .setEndpoint(endpoint)
                  .setCompression(config.compression().wireValue())
                  .setTimeout(config.batch().exportTimeout());
          config.headers().forEach(b::addHeader);
          Otlp.exporterRetryPolicy(retry)
              .ifPresentOrElse(b::setRetryPolicy, () -> b.setRetryPolicy(null));
          Otlp.applyTls(tls, b::setSslContext, b::setTrustedCertificates, b::setClientTls);
          return b.build();
        }
      default:
        throw new ConfigException(
            "tracing: the OpenTelemetry Collector has no native protocol; use grpc or"
                + " http/protobuf");
    }
  }

  /**
   * Merges an explicit tracing config over collector defaults. Fields left at the {@link
   * TracingConfig} builder default (protocol {@code http/protobuf}, sampler {@code
   * parentbased_always_on} with ratio 1, the default propagators and batch settings, gzip, no TLS,
   * no endpoint) take the collector value; headers and resource attributes merge key by key
   * (explicit wins).
   *
   * @param explicit explicit config
   * @param defaults collector config
   * @return the merged config
   */
  public static TracingConfig resolve(TracingConfig explicit, OtelCollectorConfig defaults) {
    TracingConfig.Builder b = explicit.toBuilder();
    Protocol protocol =
        explicit.protocol() == Protocol.HTTP_PROTOBUF ? defaults.protocol() : explicit.protocol();
    b.protocol(protocol);
    if (explicit.endpoint().isPresent()) {
      b.endpoint(resolveTracesEndpoint(protocol, explicit.endpoint().get()));
    } else if (protocol != defaults.protocol()) {
      b.endpoint(
          protocol == Protocol.GRPC ? defaults.grpcUrl() : defaults.signalUrl(Signal.TRACES));
    } else {
      b.endpoint(defaults.endpointFor(Signal.TRACES));
    }
    Map<String, String> headers = defaults.headersFor(Signal.TRACES);
    headers.putAll(explicit.headers());
    b.headers(headers);
    Map<String, Object> resource = new LinkedHashMap<>(defaults.resourceAttributes());
    resource.putAll(explicit.resourceAttributes());
    b.resourceAttributes(resource);
    if (explicit.serviceVersion().isEmpty()) {
      defaults.serviceVersion().ifPresent(b::serviceVersion);
    }
    if (explicit.environment().isEmpty()) {
      defaults.environment().ifPresent(b::environment);
    }
    if (explicit.sampler() == SamplerType.PARENTBASED_ALWAYS_ON && explicit.samplerRatio() == 1.0) {
      b.sampler(defaults.sampler()).samplerRatio(defaults.samplerRatio());
    }
    if (explicit.propagators().equals(TracingConfig.DEFAULT_PROPAGATORS)) {
      b.propagators(defaults.propagators());
    }
    if (explicit.batch().equals(BatchSettings.defaults())) {
      b.batch(defaults.tracesBatch());
    }
    if (explicit.compression() == Compression.GZIP) {
      b.compression(defaults.compression());
    }
    if (explicit.tls().isEmpty() && defaults.tls().isCustom()) {
      b.tls(defaults.tls());
    }
    return b.build();
  }

  /**
   * Sets up tracing from the environment ({@code GROW_OPENTELEMETRY_COLLECTOR_*}, {@code
   * GROW_OTEL_*}, {@code OTEL_*}).
   *
   * @return handle
   */
  public static TracingHandle initTracing() {
    return initTracing(OtelCollectorConfig.fromEnv());
  }

  /**
   * Sets up tracing with every setting taken from a collector config.
   *
   * @param config collector config
   * @return handle
   */
  public static TracingHandle initTracing(OtelCollectorConfig config) {
    return Tracing.initWith(config.toTracingConfig(), c -> createSpanExporter(c, config.retry()));
  }

  /**
   * Sets up tracing with an explicit config over collector defaults from the environment.
   *
   * @param config explicit tracing config
   * @return handle
   */
  public static TracingHandle initTracing(TracingConfig config) {
    return initTracing(config, OtelCollectorConfig.fromEnv());
  }

  /**
   * Sets up tracing with an explicit config over collector defaults (see {@link #resolve}).
   *
   * @param config explicit tracing config
   * @param defaults collector config
   * @return handle
   */
  public static TracingHandle initTracing(TracingConfig config, OtelCollectorConfig defaults) {
    return Tracing.initWith(
        resolve(config, defaults), c -> createSpanExporter(c, defaults.retry()));
  }
}
