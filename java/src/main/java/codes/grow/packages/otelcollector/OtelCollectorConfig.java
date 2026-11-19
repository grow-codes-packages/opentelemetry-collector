package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.EnvReader;
import codes.grow.packages.otelcollector.core.Hooks;
import codes.grow.packages.otelcollector.core.Logger;
import codes.grow.packages.otelcollector.core.Redaction;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.core.TransportConfig;
import codes.grow.packages.otelcollector.logging.BatchOptions;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.BatchSettings;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingConfig.SamplerType;
import java.net.URI;
import java.net.URISyntaxException;
import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Paths;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.EnumMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.OptionalInt;

/**
 * Immutable configuration of a {@link OtelCollectorClient} and of every OTLP exporter this package
 * builds (traces, logs, metrics).
 *
 * <p>Build one with {@link #builder()} or read it from the environment with {@link #fromEnv()}.
 * Precedence per field: explicit builder value &gt; {@code GROW_OPENTELEMETRY_COLLECTOR_<NAME>}
 * &gt; {@code GROW_OTEL_<NAME>} &gt; per-signal {@code OTEL_*} &gt; generic {@code OTEL_*} &gt;
 * default. Map fields (headers, resource attributes) merge key by key in the same order. {@link
 * #toString()} never prints credentials.
 */
public final class OtelCollectorConfig {
  /** Default OTLP/HTTP base URL (port 4318). */
  public static final String DEFAULT_URL = "http://127.0.0.1:4318";

  /** Default OTLP/gRPC target (port 4317). */
  public static final String DEFAULT_GRPC_URL = "http://127.0.0.1:4317";

  /** Default deadline of one OTLP export including the exporter's own retries. */
  public static final Duration DEFAULT_EXPORT_TIMEOUT = Duration.ofSeconds(10);

  /** Default interval of the periodic metric reader. */
  public static final Duration DEFAULT_METRIC_EXPORT_INTERVAL = Duration.ofSeconds(60);

  /** Alias prefix accepted after {@link EnvReader#DEFAULT_PREFIX} (ticket wording). */
  public static final String ALIAS_ENV_PREFIX = "GROW_OTEL_";

  /** Every resource detector, in a fixed order (the default). */
  public static final List<String> RESOURCE_DETECTORS =
      List.of("host", "process", "container", "k8s");

  /** Default {@code service.name} when none is configured (OpenTelemetry Java default). */
  public static final String DEFAULT_SERVICE_NAME = "unknown_service:java";

  /** Port of the {@code health_check} extension. */
  static final int HEALTH_PORT = 13133;

  /** Port of the zpages extension. */
  static final int ZPAGES_PORT = 55679;

  /** Port of OTLP/gRPC. */
  static final int GRPC_PORT = 4317;

  /** Variable name that can never be set: lookups that use only vendor variables. */
  private static final String NO_SDK_VARIABLE = "=";

  private final String url;
  private final Duration connectTimeout;
  private final Duration requestTimeout;
  private final Duration totalTimeout;
  private final RetryPolicy retry;
  private final TlsOptions tls;
  private final String proxy;
  private final Map<String, String> headers;
  private final String userAgentSuffix;
  private final Hooks hooks;
  private final Logger logger;
  private final String grpcUrl;
  private final Protocol protocol;
  private final Map<Signal, String> signalUrls;
  private final Map<Signal, Map<String, String>> signalHeaders;
  private final Compression compression;
  private final Duration exportTimeout;
  private final String healthUrl;
  private final String zpagesUrl;
  private final String telemetryUrl;
  private final String serviceName;
  private final String serviceVersion;
  private final String environment;
  private final Map<String, Object> resourceAttributes;
  private final boolean tracesEnabled;
  private final boolean logsEnabled;
  private final boolean metricsEnabled;
  private final SamplerType sampler;
  private final double samplerRatio;
  private final List<String> propagators;
  private final BatchSettings tracesBatch;
  private final BatchOptions logsBatch;
  private final Duration metricExportInterval;
  private final boolean runtimeMetrics;
  private final List<String> resourceDetectors;

  private OtelCollectorConfig(Builder b) {
    this.url = b.url != null ? b.url : DEFAULT_URL;
    this.connectTimeout = b.connectTimeout != null ? b.connectTimeout : Duration.ofSeconds(10);
    this.requestTimeout = b.requestTimeout != null ? b.requestTimeout : Duration.ofSeconds(30);
    this.totalTimeout = b.totalTimeout != null ? b.totalTimeout : Duration.ofSeconds(120);
    this.retry = b.retry != null ? b.retry : RetryPolicy.defaults();
    this.tls = b.tls != null ? b.tls : TlsOptions.defaults();
    this.proxy = b.proxy;
    this.headers = Collections.unmodifiableMap(new LinkedHashMap<>(b.headers));
    this.userAgentSuffix = b.userAgentSuffix;
    this.hooks = b.hooks != null ? b.hooks : Hooks.NONE;
    this.logger = b.logger != null ? b.logger : Logger.NOOP;
    this.protocol = b.protocol != null ? b.protocol : Protocol.HTTP_PROTOBUF;
    this.grpcUrl = b.grpcUrl != null ? b.grpcUrl : sibling(url, GRPC_PORT, "", DEFAULT_GRPC_URL);
    EnumMap<Signal, String> urls = new EnumMap<>(Signal.class);
    urls.putAll(b.signalUrls);
    this.signalUrls = Collections.unmodifiableMap(urls);
    EnumMap<Signal, Map<String, String>> sh = new EnumMap<>(Signal.class);
    for (Signal s : Signal.values()) {
      Map<String, String> m = b.signalHeaders.get(s);
      sh.put(s, m == null ? Map.of() : Collections.unmodifiableMap(new LinkedHashMap<>(m)));
    }
    this.signalHeaders = Collections.unmodifiableMap(sh);
    this.compression = b.compression != null ? b.compression : Compression.GZIP;
    this.exportTimeout = b.exportTimeout != null ? b.exportTimeout : DEFAULT_EXPORT_TIMEOUT;
    this.healthUrl = b.healthUrl != null ? b.healthUrl : sibling(url, HEALTH_PORT, "/", "");
    this.zpagesUrl = b.zpagesUrl != null ? b.zpagesUrl : sibling(url, ZPAGES_PORT, "", "");
    this.telemetryUrl = b.telemetryUrl;
    this.serviceName =
        b.serviceName == null || b.serviceName.isBlank()
            ? DEFAULT_SERVICE_NAME
            : b.serviceName.trim();
    this.serviceVersion = b.serviceVersion;
    this.environment = b.environment;
    this.resourceAttributes = Collections.unmodifiableMap(new LinkedHashMap<>(b.resource));
    this.tracesEnabled = b.traces == null || b.traces;
    this.logsEnabled = b.logs == null || b.logs;
    this.metricsEnabled = b.metrics == null || b.metrics;
    this.sampler = b.sampler != null ? b.sampler : SamplerType.PARENTBASED_ALWAYS_ON;
    this.samplerRatio = b.samplerRatio != null ? b.samplerRatio : 1.0;
    this.propagators =
        List.copyOf(b.propagators != null ? b.propagators : TracingConfig.DEFAULT_PROPAGATORS);
    if (b.tracesBatch != null) {
      this.tracesBatch = b.tracesBatch;
    } else {
      BatchSettings d = BatchSettings.defaults();
      this.tracesBatch =
          new BatchSettings(
              b.bspQueue != null ? b.bspQueue : d.maxQueueSize(),
              b.bspBatch != null ? b.bspBatch : d.maxExportBatchSize(),
              b.bspDelay != null ? b.bspDelay : d.scheduledDelay(),
              b.bspTimeout != null ? b.bspTimeout : exportTimeout);
    }
    if (b.logsBatch != null) {
      this.logsBatch = b.logsBatch;
    } else {
      BatchOptions.Builder lb = BatchOptions.defaults().toBuilder();
      if (b.blrpQueue != null) {
        lb.maxQueueSize(b.blrpQueue);
      }
      if (b.blrpBatch != null) {
        lb.maxBatchSize(b.blrpBatch);
      }
      if (b.blrpDelay != null) {
        lb.flushInterval(b.blrpDelay);
      }
      if (b.blrpTimeout != null) {
        lb.exportTimeout(b.blrpTimeout);
      }
      this.logsBatch = lb.build();
    }
    this.metricExportInterval =
        b.metricExportInterval != null ? b.metricExportInterval : DEFAULT_METRIC_EXPORT_INTERVAL;
    this.runtimeMetrics = b.runtimeMetrics != null && b.runtimeMetrics;
    this.resourceDetectors =
        List.copyOf(b.resourceDetectors != null ? b.resourceDetectors : RESOURCE_DETECTORS);
  }

  /**
   * Starts an empty builder (every field at its default).
   *
   * @return a new builder
   */
  public static Builder builder() {
    return new Builder();
  }

  /**
   * Reads the configuration from the process environment.
   *
   * @return the validated configuration
   * @throws ConfigException when a variable holds an invalid value (the message names the variable,
   *     never the value)
   */
  public static OtelCollectorConfig fromEnv() {
    return builder().fromEnv(EnvReader.system()).build();
  }

  /**
   * Reads the configuration from an explicit variable map (tests).
   *
   * @param env variable map
   * @return the validated configuration
   */
  public static OtelCollectorConfig fromEnv(Map<String, String> env) {
    return builder().fromEnv(EnvReader.of(env)).build();
  }

  /**
   * Returns a builder with these values (every field counts as explicit).
   *
   * @return builder copy
   */
  public Builder toBuilder() {
    Builder b = new Builder();
    b.url = url;
    b.connectTimeout = connectTimeout;
    b.requestTimeout = requestTimeout;
    b.totalTimeout = totalTimeout;
    b.retry = retry;
    b.tls = tls;
    b.proxy = proxy;
    b.headers.putAll(headers);
    b.userAgentSuffix = userAgentSuffix;
    b.hooks = hooks;
    b.logger = logger;
    b.grpcUrl = grpcUrl;
    b.protocol = protocol;
    b.signalUrls.putAll(signalUrls);
    signalHeaders.forEach((s, m) -> b.signalHeaders.put(s, new LinkedHashMap<>(m)));
    b.compression = compression;
    b.exportTimeout = exportTimeout;
    b.healthUrl = healthUrl;
    b.zpagesUrl = zpagesUrl;
    b.telemetryUrl = telemetryUrl;
    b.serviceName = serviceName;
    b.serviceVersion = serviceVersion;
    b.environment = environment;
    b.resource.putAll(resourceAttributes);
    b.traces = tracesEnabled;
    b.logs = logsEnabled;
    b.metrics = metricsEnabled;
    b.sampler = sampler;
    b.samplerRatio = samplerRatio;
    b.propagators = new ArrayList<>(propagators);
    b.tracesBatch = tracesBatch;
    b.logsBatch = logsBatch;
    b.metricExportInterval = metricExportInterval;
    b.runtimeMetrics = runtimeMetrics;
    b.resourceDetectors = new ArrayList<>(resourceDetectors);
    return b;
  }

  /**
   * Checks URLs, timeouts, protocol, sampler, batch settings, resource detectors and TLS pairs.
   *
   * @throws ConfigException when a rule is violated
   */
  public void validate() {
    if (!isHttpUrl(url)) {
      throw new ConfigException("config: url must use http or https and include a host");
    }
    requirePositive("connectTimeout", connectTimeout);
    requirePositive("requestTimeout", requestTimeout);
    requirePositive("totalTimeout", totalTimeout);
    requirePositive("exportTimeout", exportTimeout);
    requirePositive("metricExportInterval", metricExportInterval);
    tls.validate();
    if (proxy != null) {
      try {
        URI p = new URI(proxy);
        if (p.getHost() == null) {
          throw new ConfigException("config: proxy must be an absolute URL");
        }
      } catch (URISyntaxException e) {
        throw new ConfigException("config: proxy is not a valid URL");
      }
    }
    if (protocol == Protocol.NATIVE) {
      throw new ConfigException(
          "config: the OpenTelemetry Collector has no native protocol; use grpc or http/protobuf");
    }
    checkUrl("grpcUrl", grpcUrl);
    checkUrl("healthUrl", healthUrl);
    checkUrl("zpagesUrl", zpagesUrl);
    checkUrl("telemetryUrl", telemetryUrl);
    for (Map.Entry<Signal, String> e : signalUrls.entrySet()) {
      checkUrl(e.getKey().path() + "Url", e.getValue());
    }
    if (!(samplerRatio >= 0.0 && samplerRatio <= 1.0)) {
      throw new ConfigException("config: samplerRatio must be between 0 and 1");
    }
    for (String p : propagators) {
      if (p == null || p.isBlank()) {
        throw new ConfigException("config: propagators must not contain blank names");
      }
    }
    for (String d : resourceDetectors) {
      if (!RESOURCE_DETECTORS.contains(d)) {
        throw new ConfigException(
            "config: unknown resource detector " + d + " (use host, process, container or k8s)");
      }
    }
    if (tracesBatch.maxQueueSize() < 1
        || tracesBatch.maxExportBatchSize() < 1
        || tracesBatch.maxExportBatchSize() > tracesBatch.maxQueueSize()) {
      throw new ConfigException(
          "config: tracesBatch sizes must be positive and maxExportBatchSize <= maxQueueSize");
    }
    requirePositive("tracesBatch.scheduledDelay", tracesBatch.scheduledDelay());
    requirePositive("tracesBatch.exportTimeout", tracesBatch.exportTimeout());
  }

  private static void checkUrl(String name, String value) {
    if (value != null && !isHttpUrl(value)) {
      throw new ConfigException("config: " + name + " must be an absolute http or https URL");
    }
  }

  static boolean isHttpUrl(String value) {
    try {
      URI u = new URI(value);
      String scheme = u.getScheme() == null ? "" : u.getScheme().toLowerCase(Locale.ROOT);
      return (scheme.equals("http") || scheme.equals("https")) && u.getHost() != null;
    } catch (URISyntaxException | NullPointerException e) {
      return false;
    }
  }

  private static void requirePositive(String name, Duration d) {
    if (d.isZero() || d.isNegative()) {
      throw new ConfigException("config: " + name + " must be positive");
    }
  }

  /** URL with the scheme and host of {@code base} and another port and path. */
  private static String sibling(String base, int port, String path, String fallback) {
    try {
      URI u = new URI(base);
      if (u.getScheme() == null || u.getHost() == null) {
        return fallback;
      }
      return u.getScheme() + "://" + u.getHost() + ":" + port + path;
    } catch (URISyntaxException e) {
      return fallback;
    }
  }

  /**
   * Translates the common fields into settings of the shared HTTP transport (OTLP base URL, export
   * headers, TLS, retries).
   *
   * @return the transport configuration
   */
  public TransportConfig toTransportConfig() {
    return transport(url, headers);
  }

  /** Transport for the unauthenticated admin endpoints: no export headers are ever sent there. */
  TransportConfig adminTransportConfig() {
    return transport(zpagesUrl, Map.of());
  }

  private TransportConfig transport(String base, Map<String, String> hdrs) {
    return TransportConfig.builder(base)
        .connectTimeout(connectTimeout)
        .requestTimeout(requestTimeout)
        .totalTimeout(totalTimeout)
        .retry(retry)
        .tls(tls)
        .proxy(proxy)
        .headers(hdrs)
        .userAgentSuffix(userAgentSuffix)
        .hooks(hooks)
        .logger(logger)
        .knownSecrets(this::knownSecrets)
        .build();
  }

  /**
   * Values of sensitive export headers ({@code authorization}, {@code *token*}, {@code *key*}, …)
   * that are removed from every message.
   *
   * @return the secret values
   */
  public List<String> knownSecrets() {
    List<String> out = new ArrayList<>();
    List<Map<String, String>> maps = new ArrayList<>();
    maps.add(headers);
    maps.addAll(signalHeaders.values());
    for (Map<String, String> m : maps) {
      m.forEach(
          (k, v) -> {
            if (Redaction.isSensitiveHeader(k) && v != null && !v.isEmpty()) {
              out.add(v);
              int space = v.indexOf(' ');
              if (space > 0) {
                out.add(v.substring(space + 1));
              }
            }
          });
    }
    return out;
  }

  /**
   * The OTLP/HTTP URL of a signal: the explicit per-signal URL, else {@code url + /v1/<signal>}.
   *
   * @param signal the signal
   * @return the URL
   */
  public String signalUrl(Signal signal) {
    String explicit = signalUrls.get(signal);
    if (explicit != null) {
      return explicit;
    }
    return url.replaceAll("/+$", "") + "/v1/" + signal.path();
  }

  /**
   * The export endpoint of a signal for the configured protocol: the explicit per-signal URL when
   * set, else {@link #grpcUrl()} for gRPC and {@link #signalUrl(Signal)} for HTTP.
   *
   * @param signal the signal
   * @return the endpoint
   */
  public String endpointFor(Signal signal) {
    String explicit = signalUrls.get(signal);
    if (explicit != null) {
      return explicit;
    }
    return protocol == Protocol.GRPC ? grpcUrl : signalUrl(signal);
  }

  /**
   * Export headers of a signal: {@link #headers()} with the per-signal headers on top.
   *
   * @param signal the signal
   * @return a new map
   */
  public Map<String, String> headersFor(Signal signal) {
    Map<String, String> out = new LinkedHashMap<>(headers);
    out.putAll(signalHeaders.get(signal));
    return out;
  }

  /**
   * Resource attributes of the service: {@link #resourceAttributes()} plus {@code service.name},
   * {@code service.version} and {@code deployment.environment.name}.
   *
   * @return a new map
   */
  public Map<String, Object> serviceResource() {
    Map<String, Object> out = new LinkedHashMap<>(resourceAttributes);
    out.put("service.name", serviceName);
    if (serviceVersion != null) {
      out.put("service.version", serviceVersion);
    }
    if (environment != null) {
      out.put("deployment.environment.name", environment);
    }
    return out;
  }

  /**
   * The tracing settings of the traces signal: service fields, resource, endpoint, protocol,
   * headers, sampler, propagators, batch, TLS and compression.
   *
   * @return a new tracing config
   */
  public TracingConfig toTracingConfig() {
    TracingConfig.Builder b =
        TracingConfig.builder(serviceName)
            .resourceAttributes(resourceAttributes)
            .endpoint(endpointFor(Signal.TRACES))
            .protocol(protocol)
            .headers(headersFor(Signal.TRACES))
            .sampler(sampler)
            .samplerRatio(samplerRatio)
            .propagators(propagators)
            .batch(tracesBatch)
            .compression(compression);
    if (serviceVersion != null) {
      b.serviceVersion(serviceVersion);
    }
    if (environment != null) {
      b.environment(environment);
    }
    if (tls.isCustom()) {
      b.tls(tls);
    }
    return b.build();
  }

  /**
   * Returns the OTLP/HTTP base URL.
   *
   * @return the URL
   */
  public String url() {
    return url;
  }

  /**
   * Returns the connect timeout.
   *
   * @return the timeout
   */
  public Duration connectTimeout() {
    return connectTimeout;
  }

  /**
   * Returns the per-attempt timeout of health, ping and version calls.
   *
   * @return the timeout
   */
  public Duration requestTimeout() {
    return requestTimeout;
  }

  /**
   * Returns the total timeout per call including retries.
   *
   * @return the timeout
   */
  public Duration totalTimeout() {
    return totalTimeout;
  }

  /**
   * Returns the retry policy (health, ping, version; exporter retries derive from it).
   *
   * @return the policy
   */
  public RetryPolicy retry() {
    return retry;
  }

  /**
   * Returns the TLS options.
   *
   * @return the options
   */
  public TlsOptions tls() {
    return tls;
  }

  /**
   * Returns the explicit proxy URL.
   *
   * @return the proxy, or empty
   */
  public Optional<String> proxy() {
    return Optional.ofNullable(proxy);
  }

  /**
   * Returns the export headers sent with every OTLP request (auth goes here).
   *
   * @return immutable map
   */
  public Map<String, String> headers() {
    return headers;
  }

  /**
   * Returns the user agent suffix.
   *
   * @return the suffix, or empty
   */
  public Optional<String> userAgentSuffix() {
    return Optional.ofNullable(userAgentSuffix);
  }

  /**
   * Returns the hooks.
   *
   * @return the hooks
   */
  public Hooks hooks() {
    return hooks;
  }

  /**
   * Returns the logger.
   *
   * @return the logger
   */
  public Logger logger() {
    return logger;
  }

  /**
   * Returns the OTLP/gRPC target.
   *
   * @return the URL
   */
  public String grpcUrl() {
    return grpcUrl;
  }

  /**
   * Returns the export protocol ({@code http/protobuf} by default).
   *
   * @return the protocol
   */
  public Protocol protocol() {
    return protocol;
  }

  /**
   * Returns the explicit per-signal URL (used as is, no path appended).
   *
   * @param signal the signal
   * @return the URL, or empty
   */
  public Optional<String> explicitSignalUrl(Signal signal) {
    return Optional.ofNullable(signalUrls.get(signal));
  }

  /**
   * Returns the headers sent only with exports of one signal.
   *
   * @param signal the signal
   * @return immutable map
   */
  public Map<String, String> signalHeaders(Signal signal) {
    return signalHeaders.get(signal);
  }

  /**
   * Returns the export compression.
   *
   * @return the compression
   */
  public Compression compression() {
    return compression;
  }

  /**
   * Returns the deadline of one OTLP export.
   *
   * @return the timeout
   */
  public Duration exportTimeout() {
    return exportTimeout;
  }

  /**
   * Returns the {@code health_check} URL (default: host of {@link #url()}, port 13133).
   *
   * @return the URL
   */
  public String healthUrl() {
    return healthUrl;
  }

  /**
   * Returns the zpages base URL (default: host of {@link #url()}, port 55679).
   *
   * @return the URL
   */
  public String zpagesUrl() {
    return zpagesUrl;
  }

  /**
   * Returns the internal telemetry (Prometheus) URL, such as {@code http://collector:8888/metrics}.
   *
   * @return the URL, or empty
   */
  public Optional<String> telemetryUrl() {
    return Optional.ofNullable(telemetryUrl);
  }

  /**
   * Returns {@code service.name}.
   *
   * @return the name
   */
  public String serviceName() {
    return serviceName;
  }

  /**
   * Returns {@code service.version}.
   *
   * @return the version, or empty
   */
  public Optional<String> serviceVersion() {
    return Optional.ofNullable(serviceVersion);
  }

  /**
   * Returns {@code deployment.environment.name}.
   *
   * @return the environment, or empty
   */
  public Optional<String> environment() {
    return Optional.ofNullable(environment);
  }

  /**
   * Returns the resource attributes.
   *
   * @return immutable map
   */
  public Map<String, Object> resourceAttributes() {
    return resourceAttributes;
  }

  /**
   * Returns whether traces are exported.
   *
   * @return true by default
   */
  public boolean tracesEnabled() {
    return tracesEnabled;
  }

  /**
   * Returns whether logs are exported.
   *
   * @return true by default
   */
  public boolean logsEnabled() {
    return logsEnabled;
  }

  /**
   * Returns whether metrics are exported.
   *
   * @return true by default
   */
  public boolean metricsEnabled() {
    return metricsEnabled;
  }

  /**
   * Returns the trace sampler.
   *
   * @return the sampler
   */
  public SamplerType sampler() {
    return sampler;
  }

  /**
   * Returns the ratio of the {@code *traceidratio} samplers.
   *
   * @return the ratio
   */
  public double samplerRatio() {
    return samplerRatio;
  }

  /**
   * Returns the propagator names.
   *
   * @return immutable list
   */
  public List<String> propagators() {
    return propagators;
  }

  /**
   * Returns the batch span processor settings.
   *
   * @return the settings
   */
  public BatchSettings tracesBatch() {
    return tracesBatch;
  }

  /**
   * Returns the log batch settings.
   *
   * @return the options
   */
  public BatchOptions logsBatch() {
    return logsBatch;
  }

  /**
   * Returns the periodic metric reader interval.
   *
   * @return the interval
   */
  public Duration metricExportInterval() {
    return metricExportInterval;
  }

  /**
   * Returns whether JVM runtime metrics are reported.
   *
   * @return false by default
   */
  public boolean runtimeMetrics() {
    return runtimeMetrics;
  }

  /**
   * Returns the resource detectors run by {@code initTelemetry}.
   *
   * @return immutable list
   */
  public List<String> resourceDetectors() {
    return resourceDetectors;
  }

  @Override
  public String toString() {
    Map<Signal, Map<String, String>> sh = new EnumMap<>(Signal.class);
    signalHeaders.forEach((s, m) -> sh.put(s, Redaction.redactHeaders(m)));
    Map<Signal, String> urls = new EnumMap<>(Signal.class);
    signalUrls.forEach((s, u) -> urls.put(s, Redaction.redactUrl(u)));
    return "OtelCollectorConfig{url="
        + Redaction.redactUrl(url)
        + ", grpcUrl="
        + Redaction.redactUrl(grpcUrl)
        + ", protocol="
        + protocol.wireValue()
        + ", signalUrls="
        + urls
        + ", headers="
        + Redaction.redactHeaders(headers)
        + ", signalHeaders="
        + sh
        + ", compression="
        + compression.wireValue()
        + ", exportTimeout="
        + exportTimeout
        + ", healthUrl="
        + Redaction.redactUrl(healthUrl)
        + ", zpagesUrl="
        + Redaction.redactUrl(zpagesUrl)
        + ", telemetryUrl="
        + (telemetryUrl == null ? "null" : Redaction.redactUrl(telemetryUrl))
        + ", serviceName="
        + serviceName
        + ", traces="
        + tracesEnabled
        + ", logs="
        + logsEnabled
        + ", metrics="
        + metricsEnabled
        + ", sampler="
        + sampler.wireValue()
        + ", runtimeMetrics="
        + runtimeMetrics
        + ", connectTimeout="
        + connectTimeout
        + ", requestTimeout="
        + requestTimeout
        + ", totalTimeout="
        + totalTimeout
        + ", retry="
        + retry
        + ", tls="
        + tls
        + ", proxy="
        + (proxy == null ? "null" : Redaction.redactUrl(proxy))
        + "}";
  }

  /**
   * Parses {@code k=v,k2=v2} (W3C baggage style, keys and values percent-decoded) as used by {@code
   * OTEL_EXPORTER_OTLP_HEADERS} and {@code OTEL_RESOURCE_ATTRIBUTES}.
   *
   * @param raw the value
   * @param variable variable name for the error message
   * @return ordered map
   * @throws ConfigException naming {@code variable} (never the value) on malformed input
   */
  public static Map<String, String> parseKeyValueList(String raw, String variable) {
    Map<String, String> out = new LinkedHashMap<>();
    for (String part : raw.split(",")) {
      if (part.isBlank()) {
        continue;
      }
      int eq = part.indexOf('=');
      if (eq <= 0 || part.substring(0, eq).isBlank()) {
        throw invalid(variable, "key=value pairs");
      }
      try {
        out.put(decode(part.substring(0, eq).trim()), decode(part.substring(eq + 1).trim()));
      } catch (IllegalArgumentException e) {
        throw invalid(variable, "key=value pairs with valid percent-encoding");
      }
    }
    return out;
  }

  private static String decode(String s) {
    return URLDecoder.decode(s.replace("+", "%2B"), StandardCharsets.UTF_8);
  }

  private static ConfigException invalid(String variable, String expected) {
    return new ConfigException(
        "config: invalid value in " + variable + " (expected " + expected + ")");
  }

  static Protocol parseProtocol(String value, String variable) {
    switch (value.trim().toLowerCase(Locale.ROOT)) {
      case "grpc":
        return Protocol.GRPC;
      case "http/protobuf":
        return Protocol.HTTP_PROTOBUF;
      default:
        throw invalid(variable, "grpc or http/protobuf");
    }
  }

  static Compression parseCompression(String value, String variable) {
    switch (value.trim().toLowerCase(Locale.ROOT)) {
      case "gzip":
        return Compression.GZIP;
      case "none":
        return Compression.NONE;
      default:
        throw invalid(variable, "gzip or none");
    }
  }

  static SamplerType parseSampler(String value, String variable) {
    String v = value.trim().toLowerCase(Locale.ROOT);
    for (SamplerType t : SamplerType.values()) {
      if (t.wireValue().equals(v)) {
        return t;
      }
    }
    throw invalid(variable, "a known sampler name");
  }

  static double parseRatio(String value, String variable) {
    try {
      double d = Double.parseDouble(value.trim());
      if (d >= 0 && d <= 1) {
        return d;
      }
    } catch (NumberFormatException e) {
      // fall through
    }
    throw invalid(variable, "a ratio between 0 and 1");
  }

  static List<String> parseList(String value) {
    List<String> out = new ArrayList<>();
    for (String p : value.split(",")) {
      if (!p.isBlank()) {
        out.add(p.trim().toLowerCase(Locale.ROOT));
      }
    }
    return out;
  }

  static List<String> parseDetectors(String value, String variable) {
    List<String> list = parseList(value);
    if (list.equals(List.of("none"))) {
      return List.of();
    }
    for (String d : list) {
      if (!RESOURCE_DETECTORS.contains(d)) {
        throw invalid(variable, "host, process, container, k8s or none");
      }
    }
    return list;
  }

  /** Environment lookups: SDK prefix, then the alias prefix, then vendor variables. */
  private static final class Env {
    private final EnvReader r;

    Env(EnvReader r) {
      this.r = r;
    }

    private static String[] names(String name, String... vendor) {
      String[] out = new String[vendor.length + 1];
      out[0] = ALIAS_ENV_PREFIX + name;
      System.arraycopy(vendor, 0, out, 1, vendor.length);
      return out;
    }

    Optional<String> str(String name, String... vendor) {
      return r.string(name, names(name, vendor));
    }

    String source(String name, String... vendor) {
      return r.source(name, names(name, vendor)).orElse(name);
    }

    Optional<Duration> duration(String name, String... vendor) {
      Optional<Duration> d = r.duration(name, names(name, vendor));
      if (d.isPresent() && (d.get().isZero() || d.get().isNegative())) {
        throw invalid(source(name, vendor), "a positive duration");
      }
      return d;
    }

    Optional<Boolean> bool(String name, String... vendor) {
      return r.bool(name, names(name, vendor));
    }

    OptionalInt integer(String name, String... vendor) {
      return r.integer(name, names(name, vendor));
    }

    /** Only vendor variables (no SDK or alias name). */
    Optional<String> otel(String variable) {
      return r.string(NO_SDK_VARIABLE, variable);
    }

    Optional<Duration> otelDuration(String variable) {
      Optional<Duration> d = r.duration(NO_SDK_VARIABLE, variable);
      if (d.isPresent() && (d.get().isZero() || d.get().isNegative())) {
        throw invalid(variable, "a positive duration in milliseconds");
      }
      return d;
    }

    Integer otelPositiveInt(String variable) {
      OptionalInt v = r.integer(NO_SDK_VARIABLE, variable);
      if (v.isEmpty()) {
        return null;
      }
      if (v.getAsInt() <= 0) {
        throw invalid(variable, "a positive integer");
      }
      return v.getAsInt();
    }

    Map<String, String> map(String name) {
      Map<String, String> out = new LinkedHashMap<>();
      r.string(NO_SDK_VARIABLE, ALIAS_ENV_PREFIX + name)
          .ifPresent(v -> out.putAll(parseKeyValueList(v, ALIAS_ENV_PREFIX + name)));
      r.string(name)
          .ifPresent(v -> out.putAll(parseKeyValueList(v, EnvReader.DEFAULT_PREFIX + name)));
      return out;
    }
  }

  /**
   * Builder of the configuration.
   *
   * <p>Unset fields take the environment (when {@link #fromEnv(EnvReader)} is called) or the
   * default.
   */
  public static final class Builder {
    private String url;
    private Duration connectTimeout;
    private Duration requestTimeout;
    private Duration totalTimeout;
    private RetryPolicy retry;
    private TlsOptions tls;
    private String proxy;
    private final Map<String, String> headers = new LinkedHashMap<>();
    private String userAgentSuffix;
    private Hooks hooks;
    private Logger logger;
    private String grpcUrl;
    private Protocol protocol;
    private final Map<Signal, String> signalUrls = new EnumMap<>(Signal.class);
    private final Map<Signal, Map<String, String>> signalHeaders = new EnumMap<>(Signal.class);
    private Compression compression;
    private Duration exportTimeout;
    private String healthUrl;
    private String zpagesUrl;
    private String telemetryUrl;
    private String serviceName;
    private String serviceVersion;
    private String environment;
    private final Map<String, Object> resource = new LinkedHashMap<>();
    private Boolean traces;
    private Boolean logs;
    private Boolean metrics;
    private SamplerType sampler;
    private Double samplerRatio;
    private List<String> propagators;
    private BatchSettings tracesBatch;
    private BatchOptions logsBatch;
    private Duration metricExportInterval;
    private Boolean runtimeMetrics;
    private List<String> resourceDetectors;
    private Integer bspQueue;
    private Integer bspBatch;
    private Duration bspDelay;
    private Duration bspTimeout;
    private Integer blrpQueue;
    private Integer blrpBatch;
    private Duration blrpDelay;
    private Duration blrpTimeout;

    private Builder() {}

    /**
     * Fills every field that was not set explicitly from the environment: {@code
     * GROW_OPENTELEMETRY_COLLECTOR_*}, then {@code GROW_OTEL_*}, then the standard {@code OTEL_*}
     * variables.
     *
     * @param env environment reader
     * @return this builder
     * @throws ConfigException when a variable holds an invalid value
     */
    public Builder fromEnv(EnvReader env) {
      Env e = new Env(env);
      if (protocol == null) {
        protocol =
            e.str("PROTOCOL", "OTEL_EXPORTER_OTLP_PROTOCOL")
                .map(v -> parseProtocol(v, e.source("PROTOCOL", "OTEL_EXPORTER_OTLP_PROTOCOL")))
                .orElse(null);
      }
      boolean grpc = protocol == Protocol.GRPC;
      String[] endpoint = {"OTEL_EXPORTER_OTLP_ENDPOINT"};
      String[] none = {};
      if (url == null) {
        url = e.str("URL", grpc ? none : endpoint).orElse(null);
      }
      if (grpcUrl == null) {
        grpcUrl = e.str("GRPC_URL", grpc ? endpoint : none).orElse(null);
      }
      applyCommon(e);
      for (Signal s : Signal.values()) {
        String upper = s.name();
        if (!signalUrls.containsKey(s)) {
          e.str(upper + "_URL", "OTEL_EXPORTER_OTLP_" + upper + "_ENDPOINT")
              .ifPresent(v -> signalUrls.put(s, v));
        }
        String variable = "OTEL_EXPORTER_OTLP_" + upper + "_HEADERS";
        Optional<String> sh = e.otel(variable);
        if (sh.isPresent()) {
          Map<String, String> target = signalHeaders.computeIfAbsent(s, k -> new LinkedHashMap<>());
          parseKeyValueList(sh.get(), variable).forEach(target::putIfAbsent);
        }
      }
      Map<String, String> envHeaders = new LinkedHashMap<>();
      e.otel("OTEL_EXPORTER_OTLP_HEADERS")
          .ifPresent(v -> envHeaders.putAll(parseKeyValueList(v, "OTEL_EXPORTER_OTLP_HEADERS")));
      envHeaders.putAll(e.map("HEADERS"));
      envHeaders.forEach(headers::putIfAbsent);
      if (compression == null) {
        compression =
            e.str("COMPRESSION", "OTEL_EXPORTER_OTLP_COMPRESSION")
                .map(
                    v ->
                        parseCompression(
                            v, e.source("COMPRESSION", "OTEL_EXPORTER_OTLP_COMPRESSION")))
                .orElse(null);
      }
      if (exportTimeout == null) {
        exportTimeout = e.duration("EXPORT_TIMEOUT", "OTEL_EXPORTER_OTLP_TIMEOUT").orElse(null);
      }
      if (healthUrl == null) {
        healthUrl = e.str("HEALTH_URL").orElse(null);
      }
      if (zpagesUrl == null) {
        zpagesUrl = e.str("ZPAGES_URL").orElse(null);
      }
      if (telemetryUrl == null) {
        telemetryUrl = e.str("TELEMETRY_URL").orElse(null);
      }
      applyResource(e);
      applySignals(e);
      applyTracing(e);
      applyBatch(e);
      return this;
    }

    private void applyCommon(Env e) {
      if (connectTimeout == null) {
        connectTimeout = e.duration("CONNECT_TIMEOUT").orElse(null);
      }
      if (requestTimeout == null) {
        requestTimeout = e.duration("REQUEST_TIMEOUT").orElse(null);
      }
      if (totalTimeout == null) {
        totalTimeout = e.duration("TOTAL_TIMEOUT").orElse(null);
      }
      if (retry == null && e.integer("MAX_RETRIES").isPresent()) {
        int n = e.integer("MAX_RETRIES").getAsInt();
        if (n < 0) {
          throw invalid(e.source("MAX_RETRIES"), ">= 0");
        }
        retry = RetryPolicy.builder().maxRetries(n).build();
      }
      if (tls == null) {
        TlsOptions.Builder t = TlsOptions.builder();
        e.str("CA_CERT", "OTEL_EXPORTER_OTLP_CERTIFICATE")
            .ifPresent(p -> t.caCertPath(Paths.get(p)));
        e.str("CLIENT_CERT", "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE")
            .ifPresent(p -> t.clientCertPath(Paths.get(p)));
        e.str("CLIENT_KEY", "OTEL_EXPORTER_OTLP_CLIENT_KEY")
            .ifPresent(p -> t.clientKeyPath(Paths.get(p)));
        e.bool("INSECURE_SKIP_VERIFY").ifPresent(t::insecureSkipVerify);
        tls = t.build();
      }
      if (proxy == null) {
        proxy = e.str("PROXY").orElse(null);
      }
    }

    private void applyResource(Env e) {
      Map<String, String> otelAttrs = new LinkedHashMap<>();
      e.otel("OTEL_RESOURCE_ATTRIBUTES")
          .ifPresent(v -> otelAttrs.putAll(parseKeyValueList(v, "OTEL_RESOURCE_ATTRIBUTES")));
      String resourceServiceName = otelAttrs.remove("service.name");
      Map<String, String> envAttrs = new LinkedHashMap<>(otelAttrs);
      envAttrs.putAll(e.map("RESOURCE_ATTRIBUTES"));
      envAttrs.forEach(resource::putIfAbsent);
      if (serviceName == null) {
        serviceName = e.str("SERVICE_NAME", "OTEL_SERVICE_NAME").orElse(resourceServiceName);
      }
      if (serviceVersion == null) {
        serviceVersion = e.str("SERVICE_VERSION").orElse(null);
      }
      if (environment == null) {
        environment = e.str("ENVIRONMENT").orElse(null);
      }
      if (resourceDetectors == null) {
        resourceDetectors =
            e.str("RESOURCE_DETECTORS")
                .map(v -> parseDetectors(v, e.source("RESOURCE_DETECTORS")))
                .orElse(null);
      }
    }

    private void applySignals(Env e) {
      boolean disabled =
          e.otel("OTEL_SDK_DISABLED").map(v -> v.trim().equalsIgnoreCase("true")).orElse(false);
      if (traces == null) {
        traces = signal(e, "TRACES", disabled);
      }
      if (logs == null) {
        logs = signal(e, "LOGS", disabled);
      }
      if (metrics == null) {
        metrics = signal(e, "METRICS", disabled);
      }
    }

    private static Boolean signal(Env e, String name, boolean sdkDisabled) {
      Optional<Boolean> on = e.bool(name);
      if (on.isPresent()) {
        return on.get();
      }
      if (sdkDisabled) {
        return false;
      }
      Optional<String> exporter = e.otel("OTEL_" + name + "_EXPORTER");
      if (exporter.isPresent() && exporter.get().trim().equalsIgnoreCase("none")) {
        return false;
      }
      return null;
    }

    private void applyTracing(Env e) {
      Optional<String> otelSampler = e.otel("OTEL_TRACES_SAMPLER");
      if (sampler == null) {
        sampler =
            e.str("SAMPLER", "OTEL_TRACES_SAMPLER")
                .map(v -> parseSampler(v, e.source("SAMPLER", "OTEL_TRACES_SAMPLER")))
                .orElse(null);
      }
      if (samplerRatio == null) {
        Optional<String> grow = e.str("SAMPLER_RATIO");
        if (grow.isPresent()) {
          samplerRatio = parseRatio(grow.get(), e.source("SAMPLER_RATIO"));
        } else if (otelSampler.isPresent()
            && otelSampler.get().trim().toLowerCase(Locale.ROOT).endsWith("traceidratio")) {
          samplerRatio =
              e.otel("OTEL_TRACES_SAMPLER_ARG")
                  .map(v -> parseRatio(v, "OTEL_TRACES_SAMPLER_ARG"))
                  .orElse(null);
        }
      }
      if (propagators == null) {
        propagators = e.str("PROPAGATORS", "OTEL_PROPAGATORS").map(v -> parseList(v)).orElse(null);
      }
      if (metricExportInterval == null) {
        metricExportInterval =
            e.duration("METRIC_EXPORT_INTERVAL", "OTEL_METRIC_EXPORT_INTERVAL").orElse(null);
      }
      if (runtimeMetrics == null) {
        runtimeMetrics = e.bool("RUNTIME_METRICS").orElse(null);
      }
    }

    private void applyBatch(Env e) {
      if (bspDelay == null) {
        bspDelay = e.otelDuration("OTEL_BSP_SCHEDULE_DELAY").orElse(null);
      }
      if (bspTimeout == null) {
        bspTimeout = e.otelDuration("OTEL_BSP_EXPORT_TIMEOUT").orElse(null);
      }
      if (bspQueue == null) {
        bspQueue = e.otelPositiveInt("OTEL_BSP_MAX_QUEUE_SIZE");
      }
      if (bspBatch == null) {
        bspBatch = e.otelPositiveInt("OTEL_BSP_MAX_EXPORT_BATCH_SIZE");
      }
      if (blrpDelay == null) {
        blrpDelay = e.otelDuration("OTEL_BLRP_SCHEDULE_DELAY").orElse(null);
      }
      if (blrpTimeout == null) {
        blrpTimeout = e.otelDuration("OTEL_BLRP_EXPORT_TIMEOUT").orElse(null);
      }
      if (blrpQueue == null) {
        blrpQueue = e.otelPositiveInt("OTEL_BLRP_MAX_QUEUE_SIZE");
      }
      if (blrpBatch == null) {
        blrpBatch = e.otelPositiveInt("OTEL_BLRP_MAX_EXPORT_BATCH_SIZE");
      }
    }

    /**
     * Sets the OTLP/HTTP base URL (default {@code http://127.0.0.1:4318}); signal paths are
     * appended.
     *
     * @param url {@code http} or {@code https} URL
     * @return this builder
     */
    public Builder url(String url) {
      this.url = url;
      return this;
    }

    /**
     * Sets the OTLP/gRPC target (default: host of {@link #url(String)}, port 4317); {@code
     * https://} enables TLS.
     *
     * @param grpcUrl URL
     * @return this builder
     */
    public Builder grpcUrl(String grpcUrl) {
      this.grpcUrl = grpcUrl;
      return this;
    }

    /**
     * Sets the export protocol ({@code NATIVE} is rejected by {@link #build()}).
     *
     * @param protocol protocol
     * @return this builder
     */
    public Builder protocol(Protocol protocol) {
      this.protocol = protocol;
      return this;
    }

    /**
     * Sets the export protocol by name: {@code grpc}, {@code http/protobuf} or {@code native}.
     *
     * @param protocol name
     * @return this builder
     * @throws ConfigException for an unknown name or {@code native}
     */
    public Builder protocol(String protocol) {
      if ("native".equalsIgnoreCase(protocol.trim())) {
        throw new ConfigException(
            "config: the OpenTelemetry Collector has no native protocol; use grpc or"
                + " http/protobuf");
      }
      this.protocol = parseProtocol(protocol, "protocol");
      return this;
    }

    /**
     * Sets the full OTLP endpoint of one signal, used as is (no path appended).
     *
     * @param signal signal
     * @param url URL
     * @return this builder
     */
    public Builder signalUrl(Signal signal, String url) {
      this.signalUrls.put(signal, url);
      return this;
    }

    /**
     * Adds a header sent with every export and with {@code ping} (for example {@code
     * Authorization}).
     *
     * @param name header name
     * @param value header value
     * @return this builder
     */
    public Builder header(String name, String value) {
      this.headers.put(name, value);
      return this;
    }

    /**
     * Adds headers sent with every export.
     *
     * @param headers headers
     * @return this builder
     */
    public Builder headers(Map<String, String> headers) {
      this.headers.putAll(headers);
      return this;
    }

    /**
     * Adds a header sent only with exports of one signal (merged over {@link #header}).
     *
     * @param signal signal
     * @param name header name
     * @param value header value
     * @return this builder
     */
    public Builder signalHeader(Signal signal, String name, String value) {
      this.signalHeaders.computeIfAbsent(signal, k -> new LinkedHashMap<>()).put(name, value);
      return this;
    }

    /**
     * Sets the export compression (default gzip).
     *
     * @param compression compression
     * @return this builder
     */
    public Builder compression(Compression compression) {
      this.compression = compression;
      return this;
    }

    /**
     * Sets the deadline of one OTLP export including the exporter's retries (default 10 s).
     *
     * @param timeout positive duration
     * @return this builder
     */
    public Builder exportTimeout(Duration timeout) {
      this.exportTimeout = timeout;
      return this;
    }

    /**
     * Sets the {@code health_check} URL.
     *
     * @param healthUrl URL
     * @return this builder
     */
    public Builder healthUrl(String healthUrl) {
      this.healthUrl = healthUrl;
      return this;
    }

    /**
     * Sets the zpages base URL.
     *
     * @param zpagesUrl URL
     * @return this builder
     */
    public Builder zpagesUrl(String zpagesUrl) {
      this.zpagesUrl = zpagesUrl;
      return this;
    }

    /**
     * Sets the internal telemetry (Prometheus) URL used by {@code serverVersion} as fallback.
     *
     * @param telemetryUrl URL such as {@code http://collector:8888/metrics}
     * @return this builder
     */
    public Builder telemetryUrl(String telemetryUrl) {
      this.telemetryUrl = telemetryUrl;
      return this;
    }

    /**
     * Sets {@code service.name}.
     *
     * @param serviceName name
     * @return this builder
     */
    public Builder serviceName(String serviceName) {
      this.serviceName = serviceName;
      return this;
    }

    /**
     * Sets {@code service.version}.
     *
     * @param serviceVersion version
     * @return this builder
     */
    public Builder serviceVersion(String serviceVersion) {
      this.serviceVersion = serviceVersion;
      return this;
    }

    /**
     * Sets {@code deployment.environment.name}.
     *
     * @param environment environment
     * @return this builder
     */
    public Builder environment(String environment) {
      this.environment = environment;
      return this;
    }

    /**
     * Adds resource attributes (explicit keys win over environment keys).
     *
     * @param attributes attributes
     * @return this builder
     */
    public Builder resourceAttributes(Map<String, ?> attributes) {
      this.resource.putAll(attributes);
      return this;
    }

    /**
     * Adds one resource attribute.
     *
     * @param key key
     * @param value value
     * @return this builder
     */
    public Builder resourceAttribute(String key, Object value) {
      this.resource.put(key, value);
      return this;
    }

    /**
     * Turns trace export on or off (default on).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder traces(boolean enabled) {
      this.traces = enabled;
      return this;
    }

    /**
     * Turns log export on or off (default on).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder logs(boolean enabled) {
      this.logs = enabled;
      return this;
    }

    /**
     * Turns metric export on or off (default on).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder metrics(boolean enabled) {
      this.metrics = enabled;
      return this;
    }

    /**
     * Sets the trace sampler (default {@code parentbased_always_on}).
     *
     * @param sampler sampler
     * @return this builder
     */
    public Builder sampler(SamplerType sampler) {
      this.sampler = sampler;
      return this;
    }

    /**
     * Sets the ratio of the {@code *traceidratio} samplers (default 1).
     *
     * @param ratio ratio between 0 and 1
     * @return this builder
     */
    public Builder samplerRatio(double ratio) {
      this.samplerRatio = ratio;
      return this;
    }

    /**
     * Sets the propagators (default {@code tracecontext}, {@code baggage}).
     *
     * @param propagators names
     * @return this builder
     */
    public Builder propagators(List<String> propagators) {
      this.propagators = new ArrayList<>(propagators);
      return this;
    }

    /**
     * Sets the propagators.
     *
     * @param propagators names
     * @return this builder
     */
    public Builder propagators(String... propagators) {
      return propagators(Arrays.asList(propagators));
    }

    /**
     * Sets the batch span processor settings ({@code OTEL_BSP_*} are then ignored).
     *
     * @param batch settings
     * @return this builder
     */
    public Builder tracesBatch(BatchSettings batch) {
      this.tracesBatch = batch;
      return this;
    }

    /**
     * Sets the log batch settings ({@code OTEL_BLRP_*} are then ignored).
     *
     * @param batch options
     * @return this builder
     */
    public Builder logsBatch(BatchOptions batch) {
      this.logsBatch = batch;
      return this;
    }

    /**
     * Sets the periodic metric reader interval (default 60 s).
     *
     * @param interval positive duration
     * @return this builder
     */
    public Builder metricExportInterval(Duration interval) {
      this.metricExportInterval = interval;
      return this;
    }

    /**
     * Turns JVM runtime metrics on or off (default off).
     *
     * @param enabled on
     * @return this builder
     */
    public Builder runtimeMetrics(boolean enabled) {
      this.runtimeMetrics = enabled;
      return this;
    }

    /**
     * Sets the resource detectors ({@code host}, {@code process}, {@code container}, {@code k8s});
     * an empty list disables detection.
     *
     * @param detectors names
     * @return this builder
     */
    public Builder resourceDetectors(List<String> detectors) {
      this.resourceDetectors = new ArrayList<>(detectors);
      return this;
    }

    /**
     * Sets the connect timeout (default 10 s).
     *
     * @param d positive duration
     * @return this builder
     */
    public Builder connectTimeout(Duration d) {
      this.connectTimeout = d;
      return this;
    }

    /**
     * Sets the per-attempt timeout of health, ping and version calls (default 30 s).
     *
     * @param d positive duration
     * @return this builder
     */
    public Builder requestTimeout(Duration d) {
      this.requestTimeout = d;
      return this;
    }

    /**
     * Sets the total timeout per call including retries (default 120 s).
     *
     * @param d positive duration
     * @return this builder
     */
    public Builder totalTimeout(Duration d) {
      this.totalTimeout = d;
      return this;
    }

    /**
     * Sets the retry policy (default {@link RetryPolicy#defaults()}).
     *
     * @param retry policy
     * @return this builder
     */
    public Builder retry(RetryPolicy retry) {
      this.retry = retry;
      return this;
    }

    /**
     * Sets the TLS options; environment TLS variables are then ignored.
     *
     * @param tls options
     * @return this builder
     */
    public Builder tls(TlsOptions tls) {
      this.tls = tls;
      return this;
    }

    /**
     * Sets an explicit proxy URL for health, ping and version calls.
     *
     * @param proxy proxy URL
     * @return this builder
     */
    public Builder proxy(String proxy) {
      this.proxy = proxy;
      return this;
    }

    /**
     * Sets the text appended to the {@code User-Agent}.
     *
     * @param suffix suffix
     * @return this builder
     */
    public Builder userAgentSuffix(String suffix) {
      this.userAgentSuffix = suffix;
      return this;
    }

    /**
     * Sets request hooks.
     *
     * @param hooks hooks
     * @return this builder
     */
    public Builder hooks(Hooks hooks) {
      this.hooks = hooks;
      return this;
    }

    /**
     * Sets the logger (default silent).
     *
     * @param logger logger
     * @return this builder
     */
    public Builder logger(Logger logger) {
      this.logger = logger;
      return this;
    }

    /**
     * Builds and validates the configuration.
     *
     * @return the configuration
     * @throws ConfigException when a rule is violated
     */
    public OtelCollectorConfig build() {
      OtelCollectorConfig c = new OtelCollectorConfig(this);
      c.validate();
      return c;
    }
  }
}
