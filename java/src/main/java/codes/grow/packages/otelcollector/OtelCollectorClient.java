package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.AuthenticationException;
import codes.grow.packages.otelcollector.core.CancelledException;
import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.EnvReader;
import codes.grow.packages.otelcollector.core.HealthStatus;
import codes.grow.packages.otelcollector.core.Hooks;
import codes.grow.packages.otelcollector.core.HttpTransport;
import codes.grow.packages.otelcollector.core.Logger;
import codes.grow.packages.otelcollector.core.NotFoundException;
import codes.grow.packages.otelcollector.core.PermissionDeniedException;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.core.TransportRequest;
import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.logging.BatchOptions;
import codes.grow.packages.otelcollector.logging.BatchingLogClient;
import codes.grow.packages.otelcollector.logging.LogQueryClient;
import codes.grow.packages.otelcollector.tracing.TracingConfig;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import codes.grow.packages.otelcollector.tracing.TracingHandle;
import codes.grow.packages.otelcollector.tracing.query.TraceQueryClient;
import io.opentelemetry.sdk.common.export.GrpcResponse;
import io.opentelemetry.sdk.common.export.GrpcSender;
import io.opentelemetry.sdk.common.export.GrpcStatusCode;
import io.opentelemetry.sdk.common.export.MessageWriter;
import java.io.OutputStream;
import java.net.URI;
import java.time.Duration;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Client for an OpenTelemetry Collector: health ({@code health_check} with zpages fallback), an
 * authenticated OTLP {@code ping}, the collector version, and factories for the telemetry
 * bootstrap, tracing, log clients and exporters whose lifetime the client owns ({@link #close()}
 * flushes and shuts them down).
 *
 * <pre>{@code
 * try (OtelCollectorClient client = OtelCollectorClient.fromEnv()) {
 *   HealthStatus health = client.health();
 *   TelemetryHandle telemetry = client.initTelemetry();
 * }
 * }</pre>
 *
 * <p>Every I/O method has a blocking form and an {@code …Async} form returning {@link
 * CompletableFuture}. Thread-safe.
 */
public final class OtelCollectorClient implements AutoCloseable {
  private static final int[] TERMINAL_4XX = {
    400, 401, 403, 404, 405, 406, 409, 410, 411, 412, 413, 414, 415, 422
  };

  private final OtelCollectorConfig config;
  private final HttpTransport transport;
  private final CollectorHealthClient health;
  private final AtomicBoolean closed = new AtomicBoolean();
  private final List<AutoCloseable> resources = new ArrayList<>();
  private GrpcSender grpcSender;

  private OtelCollectorClient(OtelCollectorConfig config) {
    this.config = config;
    this.transport = new HttpTransport(config.toTransportConfig());
    this.health = new CollectorHealthClient(config, CollectorHealthClient.DEFAULT_READY_INTERVAL);
  }

  /**
   * Creates a client.
   *
   * @param config validated configuration
   * @return a new client
   * @throws ConfigException when the configuration is invalid
   */
  public static OtelCollectorClient create(OtelCollectorConfig config) {
    config.validate();
    return new OtelCollectorClient(config);
  }

  /**
   * Creates a client from {@code GROW_OPENTELEMETRY_COLLECTOR_*}, {@code GROW_OTEL_*} and the
   * standard {@code OTEL_*} variables.
   *
   * @return a new client
   */
  public static OtelCollectorClient fromEnv() {
    return create(OtelCollectorConfig.fromEnv());
  }

  /**
   * Starts a client builder.
   *
   * @return a new builder
   */
  public static Builder builder() {
    return new Builder();
  }

  /**
   * Returns the configuration.
   *
   * @return the configuration
   */
  public OtelCollectorConfig config() {
    return config;
  }

  /**
   * Returns the HTTP transport (OTLP base URL, export headers), for service modules.
   *
   * @return the transport
   */
  public HttpTransport transport() {
    return transport;
  }

  /**
   * Registers a resource closed with the client.
   *
   * @param resource the resource
   * @throws CancelledException when the client is closed (the resource is closed first)
   */
  public void register(AutoCloseable resource) {
    synchronized (resources) {
      if (!closed.get()) {
        resources.add(resource);
        return;
      }
    }
    closeQuietly(resource);
    throw new CancelledException("client is closed");
  }

  /**
   * Returns the health client (closed with this client).
   *
   * @return the health client
   */
  public CollectorHealthClient collectorHealth() {
    return health;
  }

  /**
   * Unauthenticated health: {@code GET {healthUrl}} (port 13133), falling back to zpages when the
   * health endpoint is unreachable. Never throws for an unhealthy or unreachable collector.
   *
   * @return the status
   * @throws CancelledException when interrupted or closed
   */
  public HealthStatus health() {
    return health.health();
  }

  /**
   * Asynchronous {@link #health()}.
   *
   * @return a future with the status
   */
  public CompletableFuture<HealthStatus> healthAsync() {
    return health.healthAsync();
  }

  /**
   * Polls {@link #health()} until healthy.
   *
   * @param timeout deadline
   * @return the healthy status
   * @throws codes.grow.packages.otelcollector.core.TimeoutException at the deadline
   */
  public HealthStatus waitUntilReady(Duration timeout) {
    return health.waitUntilReady(timeout);
  }

  /**
   * Asynchronous {@link #waitUntilReady(Duration)}.
   *
   * @param timeout deadline
   * @return a future with the healthy status
   */
  public CompletableFuture<HealthStatus> waitUntilReadyAsync(Duration timeout) {
    return health.waitUntilReadyAsync(timeout);
  }

  /**
   * Authenticated round trip: an empty OTLP trace export (creates no telemetry) with the configured
   * headers: {@code POST {tracesUrl}} with an empty protobuf body (HTTP) or {@code
   * TraceService/Export} with a 0-byte message (gRPC).
   *
   * @return the latency
   * @throws AuthenticationException on 401 / gRPC UNAUTHENTICATED
   * @throws PermissionDeniedException on 403 / gRPC PERMISSION_DENIED
   * @throws NotFoundException for a wrong path / gRPC UNIMPLEMENTED
   */
  public Duration ping() {
    return HttpTransport.await(pingAsync(), "ping");
  }

  /**
   * Asynchronous {@link #ping()}.
   *
   * @return a future with the latency
   */
  public CompletableFuture<Duration> pingAsync() {
    if (closed.get()) {
      return CompletableFuture.failedFuture(new CancelledException("ping: client is closed"));
    }
    long start = System.nanoTime();
    CompletableFuture<Void> call = config.protocol() == Protocol.GRPC ? grpcPing() : httpPing();
    return call.thenApply(v -> Duration.ofNanos(System.nanoTime() - start));
  }

  private CompletableFuture<Void> httpPing() {
    TransportRequest.Builder b =
        TransportRequest.post(config.signalUrl(Signal.TRACES))
            .operation("ping")
            .body(new byte[0], "application/x-protobuf")
            .header("Accept", "application/x-protobuf")
            .idempotent(true)
            .acceptStatus(TERMINAL_4XX);
    config.signalHeaders(Signal.TRACES).forEach(b::header);
    return transport
        .sendAsync(b.build())
        .thenApply(
            r -> {
              if (r.status() / 100 != 2) {
                throw Otlp.httpError("ping", r.status(), r.body(), config.knownSecrets());
              }
              return null;
            });
  }

  private synchronized GrpcSender grpcSender() {
    if (grpcSender == null) {
      Map<String, List<String>> headers = new LinkedHashMap<>();
      config
          .headersFor(Signal.TRACES)
          .forEach((k, v) -> headers.put(k.toLowerCase(Locale.ROOT), List.of(v)));
      URI endpoint = URI.create(config.endpointFor(Signal.TRACES));
      boolean https = "https".equalsIgnoreCase(endpoint.getScheme());
      TlsOptions tls = config.tls();
      grpcSender =
          OtlpSenders.grpcSender(
              endpoint,
              Otlp.GRPC_TRACES_METHOD,
              config.requestTimeout(),
              config.connectTimeout(),
              headers,
              Otlp.exporterRetryPolicy(config.retry()).orElse(null),
              https && tls.isCustom() ? tls.sslContext() : null,
              https && tls.isCustom() ? Otlp.trustManager(tls) : null);
    }
    return grpcSender;
  }

  private CompletableFuture<Void> grpcPing() {
    CompletableFuture<Void> f = new CompletableFuture<>();
    GrpcSender sender;
    try {
      sender = grpcSender();
    } catch (RuntimeException e) {
      return CompletableFuture.failedFuture(e);
    }
    List<String> secrets = config.knownSecrets();
    sender.send(
        EMPTY,
        (GrpcResponse r) -> {
          if (r.getStatusCode() == GrpcStatusCode.OK) {
            f.complete(null);
          } else {
            f.completeExceptionally(
                Otlp.grpcError(
                    "ping", r.getStatusCode().getValue(), r.getStatusDescription(), secrets));
          }
        },
        e -> f.completeExceptionally(Otlp.networkError("ping", e, secrets)));
    return f;
  }

  private static final MessageWriter EMPTY =
      new MessageWriter() {
        @Override
        public void writeMessage(OutputStream out) {}

        @Override
        public int getContentLength() {
          return 0;
        }
      };

  /**
   * Collector version from zpages {@code /debug/servicez} (works on stock stacks), else {@code
   * target_info} at {@code telemetryUrl}.
   *
   * @return the version, such as {@code 0.165.0}
   * @throws UnsupportedException when neither source is reachable
   */
  public String serverVersion() {
    return health.serverVersion();
  }

  /**
   * Asynchronous {@link #serverVersion()}.
   *
   * @return a future with the version
   */
  public CompletableFuture<String> serverVersionAsync() {
    if (closed.get()) {
      return CompletableFuture.failedFuture(
          new CancelledException("serverVersion: client is closed"));
    }
    return health.serverVersionAsync();
  }

  /**
   * {@link OtelCollectorTelemetry#initTelemetry(OtelCollectorConfig)} with this client's config;
   * {@link #close()} shuts the handle down.
   *
   * @return the handle
   */
  public TelemetryHandle initTelemetry() {
    return initTelemetry(TelemetryOptions.defaults());
  }

  /**
   * {@link OtelCollectorTelemetry#initTelemetry(OtelCollectorConfig, TelemetryOptions)} with this
   * client's config; {@link #close()} shuts the handle down.
   *
   * @param options options
   * @return the handle
   */
  public TelemetryHandle initTelemetry(TelemetryOptions options) {
    TelemetryHandle h = OtelCollectorTelemetry.initTelemetry(config, options);
    register(h);
    return h;
  }

  /**
   * Tracing with every setting from this client's config; {@link #close()} shuts it down.
   *
   * @return the handle
   */
  public TracingHandle initTracing() {
    TracingHandle h = OtelCollectorTracing.initTracing(config);
    register(h);
    return h;
  }

  /**
   * Tracing with an explicit config over this client's config (see {@link
   * OtelCollectorTracing#resolve}); {@link #close()} shuts it down.
   *
   * @param tracing explicit tracing config
   * @return the handle
   */
  public TracingHandle initTracing(TracingConfig tracing) {
    TracingHandle h = OtelCollectorTracing.initTracing(tracing, config);
    register(h);
    return h;
  }

  /**
   * An OTLP log client with the configured batch options; {@link #close()} flushes and closes it.
   *
   * @return the log client
   */
  public BatchingLogClient logClient() {
    BatchingLogClient c = OtelCollectorLogs.createLogClient(config);
    register(c);
    return c;
  }

  /**
   * An OTLP log client with explicit batch options; {@link #close()} flushes and closes it.
   *
   * @param batch batch options
   * @return the log client
   */
  public BatchingLogClient logClient(BatchOptions batch) {
    BatchingLogClient c = OtelCollectorLogs.createLogClient(config, batch);
    register(c);
    return c;
  }

  /**
   * A bare OTLP log exporter for this config; {@link #close()} shuts it down.
   *
   * @return the exporter
   */
  public OtlpLogExporter logExporter() {
    OtlpLogExporter e = new OtlpLogExporter(config);
    register(() -> e.shutdown().join());
    return e;
  }

  /**
   * Trace query client: the collector has none, every method throws {@link UnsupportedException}.
   *
   * @return the client
   */
  public TraceQueryClient traceQuery() {
    return new CollectorTraceQueryClient();
  }

  /**
   * Log query client: the collector has none, every method throws {@link UnsupportedException}.
   *
   * @return the client
   */
  public LogQueryClient logQuery() {
    return new CollectorLogQueryClient();
  }

  /**
   * Returns whether {@link #close()} was called.
   *
   * @return true once closed
   */
  public boolean isClosed() {
    return closed.get();
  }

  /**
   * Flushes and shuts down every handle, log client and exporter this client created (newest
   * first), then releases connections. Idempotent.
   */
  @Override
  public void close() {
    List<AutoCloseable> copy;
    synchronized (resources) {
      if (!closed.compareAndSet(false, true)) {
        return;
      }
      copy = new ArrayList<>(resources);
      resources.clear();
    }
    for (int i = copy.size() - 1; i >= 0; i--) {
      closeQuietly(copy.get(i));
    }
    GrpcSender sender;
    synchronized (this) {
      sender = grpcSender;
      grpcSender = null;
    }
    if (sender != null) {
      sender.shutdown();
    }
    transport.close();
    health.close();
  }

  private static void closeQuietly(AutoCloseable r) {
    try {
      r.close();
    } catch (Exception ignored) {
      // best effort: close must not throw
    }
  }

  @Override
  public String toString() {
    return "OtelCollectorClient{" + config + "}";
  }

  /** Builder that configures and creates a client in one chain. */
  public static final class Builder {
    private final OtelCollectorConfig.Builder config = OtelCollectorConfig.builder();

    private Builder() {}

    /**
     * Fills unset fields from an environment reader.
     *
     * @param env reader, e.g. {@link EnvReader#system()}
     * @return this builder
     */
    public Builder fromEnv(EnvReader env) {
      config.fromEnv(env);
      return this;
    }

    /**
     * Sets the OTLP/HTTP base URL.
     *
     * @param url URL
     * @return this builder
     */
    public Builder url(String url) {
      config.url(url);
      return this;
    }

    /**
     * Sets the OTLP/gRPC target.
     *
     * @param url URL
     * @return this builder
     */
    public Builder grpcUrl(String url) {
      config.grpcUrl(url);
      return this;
    }

    /**
     * Sets the export protocol.
     *
     * @param protocol protocol
     * @return this builder
     */
    public Builder protocol(Protocol protocol) {
      config.protocol(protocol);
      return this;
    }

    /**
     * Sets the {@code health_check} URL.
     *
     * @param url URL
     * @return this builder
     */
    public Builder healthUrl(String url) {
      config.healthUrl(url);
      return this;
    }

    /**
     * Sets the zpages base URL.
     *
     * @param url URL
     * @return this builder
     */
    public Builder zpagesUrl(String url) {
      config.zpagesUrl(url);
      return this;
    }

    /**
     * Sets the internal telemetry URL.
     *
     * @param url URL
     * @return this builder
     */
    public Builder telemetryUrl(String url) {
      config.telemetryUrl(url);
      return this;
    }

    /**
     * Sets {@code service.name}.
     *
     * @param name name
     * @return this builder
     */
    public Builder serviceName(String name) {
      config.serviceName(name);
      return this;
    }

    /**
     * Sets the connect timeout.
     *
     * @param d duration
     * @return this builder
     */
    public Builder connectTimeout(Duration d) {
      config.connectTimeout(d);
      return this;
    }

    /**
     * Sets the per-attempt timeout.
     *
     * @param d duration
     * @return this builder
     */
    public Builder requestTimeout(Duration d) {
      config.requestTimeout(d);
      return this;
    }

    /**
     * Sets the total timeout.
     *
     * @param d duration
     * @return this builder
     */
    public Builder totalTimeout(Duration d) {
      config.totalTimeout(d);
      return this;
    }

    /**
     * Sets the retry policy.
     *
     * @param retry policy
     * @return this builder
     */
    public Builder retry(RetryPolicy retry) {
      config.retry(retry);
      return this;
    }

    /**
     * Sets the TLS options.
     *
     * @param tls options
     * @return this builder
     */
    public Builder tls(TlsOptions tls) {
      config.tls(tls);
      return this;
    }

    /**
     * Sets the proxy URL.
     *
     * @param proxy URL
     * @return this builder
     */
    public Builder proxy(String proxy) {
      config.proxy(proxy);
      return this;
    }

    /**
     * Adds an export header (for example {@code Authorization}).
     *
     * @param name name
     * @param value value
     * @return this builder
     */
    public Builder header(String name, String value) {
      config.header(name, value);
      return this;
    }

    /**
     * Sets the user agent suffix.
     *
     * @param suffix suffix
     * @return this builder
     */
    public Builder userAgentSuffix(String suffix) {
      config.userAgentSuffix(suffix);
      return this;
    }

    /**
     * Sets hooks.
     *
     * @param hooks hooks
     * @return this builder
     */
    public Builder hooks(Hooks hooks) {
      config.hooks(hooks);
      return this;
    }

    /**
     * Sets the logger.
     *
     * @param logger logger
     * @return this builder
     */
    public Builder logger(Logger logger) {
      config.logger(logger);
      return this;
    }

    /**
     * Builds the configuration and the client.
     *
     * @return a new client
     * @throws ConfigException when the configuration is invalid
     */
    public OtelCollectorClient build() {
      return create(config.build());
    }
  }
}
