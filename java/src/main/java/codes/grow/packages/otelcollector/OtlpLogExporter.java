package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.CancelledException;
import codes.grow.packages.otelcollector.core.OtelCollectorException;
import codes.grow.packages.otelcollector.core.SdkInfo;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.ValidationException;
import codes.grow.packages.otelcollector.logging.ExportOptions;
import codes.grow.packages.otelcollector.logging.ExportResult;
import codes.grow.packages.otelcollector.logging.LogExporter;
import codes.grow.packages.otelcollector.logging.LogRecord;
import codes.grow.packages.otelcollector.tracing.Tracing;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Compression;
import codes.grow.packages.otelcollector.tracing.TracingConfig.Protocol;
import io.opentelemetry.api.common.Attributes;
import io.opentelemetry.api.common.Value;
import io.opentelemetry.api.logs.Severity;
import io.opentelemetry.api.trace.SpanContext;
import io.opentelemetry.api.trace.TraceFlags;
import io.opentelemetry.api.trace.TraceState;
import io.opentelemetry.common.ComponentLoader;
import io.opentelemetry.exporter.otlp.http.logs.OtlpHttpLogRecordExporter;
import io.opentelemetry.exporter.otlp.http.logs.OtlpHttpLogRecordExporterBuilder;
import io.opentelemetry.exporter.otlp.logs.OtlpGrpcLogRecordExporter;
import io.opentelemetry.exporter.otlp.logs.OtlpGrpcLogRecordExporterBuilder;
import io.opentelemetry.sdk.common.CompletableResultCode;
import io.opentelemetry.sdk.common.InstrumentationScopeInfo;
import io.opentelemetry.sdk.common.export.GrpcResponse;
import io.opentelemetry.sdk.common.export.GrpcStatusCode;
import io.opentelemetry.sdk.common.export.HttpResponse;
import io.opentelemetry.sdk.logs.data.Body;
import io.opentelemetry.sdk.logs.data.LogRecordData;
import io.opentelemetry.sdk.logs.export.LogRecordExporter;
import io.opentelemetry.sdk.resources.Resource;
import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * {@link LogExporter} (SPEC section 4) that sends records to the collector over OTLP with the
 * official OpenTelemetry log exporter: {@code POST {logsUrl}} ({@code application/x-protobuf}) or
 * gRPC {@code LogsService/Export}. Retries (HTTP 429/502/503/504, retryable gRPC codes) are done by
 * the exporter within {@link OtelCollectorConfig#exportTimeout()}. A partial success reports {@code
 * rejected} with a {@link ValidationException}.
 *
 * <p>Exports run one at a time (the outcome of each request is read from the exporter's sender).
 */
public final class OtlpLogExporter implements LogExporter {
  /** Instrumentation scope of records exported by this class. */
  public static final InstrumentationScopeInfo SCOPE =
      InstrumentationScopeInfo.builder(Tracing.INSTRUMENTATION_NAME)
          .setVersion(SdkInfo.VERSION)
          .build();

  private static final String OP = "logs.export";

  private final OtelCollectorConfig config;
  private final String endpoint;
  private final AtomicReference<Object[]> outcome = new AtomicReference<>();
  private final Map<Boolean, LogRecordExporter> exporters = new LinkedHashMap<>();
  private final Map<Map<String, Object>, Resource> resources = new LinkedHashMap<>();
  private CompletableFuture<?> tail = CompletableFuture.completedFuture(null);
  private boolean shutdown;

  /**
   * Creates an exporter.
   *
   * @param config collector config (endpoint, protocol, headers, compression, TLS, timeout)
   */
  public OtlpLogExporter(OtelCollectorConfig config) {
    this.config = config;
    this.endpoint = config.endpointFor(Signal.LOGS);
  }

  /**
   * Returns the endpoint records are sent to ({@code logsUrl} for HTTP, {@code grpcUrl} for gRPC).
   *
   * @return the endpoint
   */
  public String endpoint() {
    return endpoint;
  }

  /**
   * Builds the official OTLP log record exporter for a collector config (also used for the {@code
   * LoggerProvider} of {@code initTelemetry}).
   *
   * @param config collector config
   * @param gzip whether to compress (only when the config compression is gzip)
   * @return the exporter
   */
  public static LogRecordExporter officialExporter(OtelCollectorConfig config, boolean gzip) {
    return officialExporter(config, gzip, null);
  }

  static LogRecordExporter officialExporter(
      OtelCollectorConfig config, boolean gzip, ComponentLoader loader) {
    String url = config.endpointFor(Signal.LOGS);
    String compression =
        gzip && config.compression() == Compression.GZIP ? "gzip" : Compression.NONE.wireValue();
    if (config.protocol() == Protocol.GRPC) {
      OtlpGrpcLogRecordExporterBuilder b =
          OtlpGrpcLogRecordExporter.builder()
              .setEndpoint(url)
              .setCompression(compression)
              .setTimeout(config.exportTimeout())
              .setConnectTimeout(config.connectTimeout());
      config.headersFor(Signal.LOGS).forEach(b::addHeader);
      Otlp.exporterRetryPolicy(config.retry())
          .ifPresentOrElse(b::setRetryPolicy, () -> b.setRetryPolicy(null));
      Otlp.applyTls(config.tls(), b::setSslContext, b::setTrustedCertificates, b::setClientTls);
      if (loader != null) {
        b.setComponentLoader(loader);
      }
      return b.build();
    }
    OtlpHttpLogRecordExporterBuilder b =
        OtlpHttpLogRecordExporter.builder()
            .setEndpoint(url)
            .setCompression(compression)
            .setTimeout(config.exportTimeout())
            .setConnectTimeout(config.connectTimeout());
    config.headersFor(Signal.LOGS).forEach(b::addHeader);
    Otlp.exporterRetryPolicy(config.retry())
        .ifPresentOrElse(b::setRetryPolicy, () -> b.setRetryPolicy(null));
    Otlp.applyTls(config.tls(), b::setSslContext, b::setTrustedCertificates, b::setClientTls);
    if (loader != null) {
      b.setComponentLoader(loader);
    }
    return b.build();
  }

  private LogRecordExporter exporter(boolean gzip) {
    return exporters.computeIfAbsent(
        gzip,
        g ->
            officialExporter(
                config, g, OtlpSenders.capturing((r, e) -> outcome.set(new Object[] {r, e}))));
  }

  private Resource resource(Map<String, Object> attributes) {
    Resource r = resources.get(attributes);
    if (r == null) {
      if (resources.size() >= 64) {
        resources.clear();
      }
      r = Resource.create(OtelValues.attributes(attributes));
      resources.put(attributes, r);
    }
    return r;
  }

  /**
   * Exports {@code records} in one OTLP request. The future never fails: failures are reported in
   * {@link ExportResult#error()} with every record counted as rejected.
   *
   * @param records records
   * @param options gzip and deadline
   * @return the result
   */
  @Override
  public synchronized CompletableFuture<ExportResult> export(
      List<LogRecord> records, ExportOptions options) {
    int n = records.size();
    if (n == 0) {
      return CompletableFuture.completedFuture(ExportResult.success(0));
    }
    if (shutdown) {
      return CompletableFuture.completedFuture(
          ExportResult.failure(0, n, new CancelledException(OP + ": exporter is shut down")));
    }
    List<LogRecordData> data;
    LogRecordExporter exp;
    try {
      exp = exporter(options.gzip());
      data = new ArrayList<>(n);
      for (LogRecord r : records) {
        data.add(new Data(r, resource(r.resource())));
      }
    } catch (OtelCollectorException e) {
      return CompletableFuture.completedFuture(ExportResult.failure(0, n, e));
    }
    CompletableFuture<ExportResult> result =
        tail.handle((v, t) -> null).thenCompose(v -> send(exp, data, n));
    tail = result;
    return result
        .copy()
        .orTimeout(options.timeout().toNanos(), TimeUnit.NANOSECONDS)
        .exceptionally(
            t ->
                ExportResult.failure(
                    0,
                    n,
                    new TimeoutException(
                        OP + ": not completed within " + options.timeout().toMillis() + " ms")));
  }

  private CompletableFuture<ExportResult> send(
      LogRecordExporter exp, List<LogRecordData> data, int n) {
    outcome.set(null);
    CompletableFuture<ExportResult> f = new CompletableFuture<>();
    CompletableResultCode code = exp.export(data);
    code.whenComplete(
        () -> {
          try {
            f.complete(result(code.isSuccess(), n));
          } catch (RuntimeException e) {
            f.complete(ExportResult.failure(0, n, Otlp.networkError(OP, e, List.of())));
          }
        });
    return f;
  }

  private ExportResult result(boolean success, int n) {
    Object[] o = outcome.getAndSet(null);
    Object response = o == null ? null : o[0];
    Throwable error = o == null ? null : (Throwable) o[1];
    List<String> secrets = config.knownSecrets();
    if (success) {
      byte[] body = null;
      if (response instanceof HttpResponse) {
        body = ((HttpResponse) response).getResponseBody();
      } else if (response instanceof GrpcResponse) {
        body = ((GrpcResponse) response).getResponseMessage();
      }
      Otlp.PartialSuccess ps = Otlp.decodePartialSuccess(body);
      int rejected = (int) Math.min(n, ps.rejected());
      if (rejected == 0) {
        return ExportResult.success(n);
      }
      String msg = OP + ": the collector rejected " + rejected + " of " + n + " records";
      if (!ps.message().isEmpty()) {
        msg += " (" + ps.message() + ")";
      }
      return ExportResult.failure(
          n - rejected,
          rejected,
          new ValidationException(
              codes.grow.packages.otelcollector.core.Redaction.truncate(
                  codes.grow.packages.otelcollector.core.Redaction.redactText(msg, secrets), 400)));
    }
    OtelCollectorException e;
    if (response instanceof HttpResponse) {
      HttpResponse r = (HttpResponse) response;
      e = Otlp.httpError(OP, r.getStatusCode(), r.getResponseBody(), secrets);
    } else if (response instanceof GrpcResponse
        && ((GrpcResponse) response).getStatusCode() != GrpcStatusCode.OK) {
      GrpcResponse r = (GrpcResponse) response;
      e = Otlp.grpcError(OP, r.getStatusCode().getValue(), r.getStatusDescription(), secrets);
    } else {
      e = Otlp.networkError(OP, error, secrets);
    }
    return ExportResult.failure(0, n, e);
  }

  /**
   * Waits for in-flight exports and releases connections. Idempotent.
   *
   * @return a future completed when shut down
   */
  @Override
  public synchronized CompletableFuture<Void> shutdown() {
    if (shutdown) {
      return CompletableFuture.completedFuture(null);
    }
    shutdown = true;
    List<LogRecordExporter> list = new ArrayList<>(exporters.values());
    exporters.clear();
    return tail.handle((v, t) -> null)
        .thenCompose(
            v -> {
              List<CompletableResultCode> codes = new ArrayList<>();
              for (LogRecordExporter e : list) {
                codes.add(e.shutdown());
              }
              CompletableFuture<Void> f = new CompletableFuture<>();
              CompletableResultCode.ofAll(codes).whenComplete(() -> f.complete(null));
              return f;
            });
  }

  /** OpenTelemetry view of one SDK log record. */
  private static final class Data implements LogRecordData {
    private final LogRecord r;
    private final Resource resource;
    private final Attributes attributes;
    private final SpanContext spanContext;

    Data(LogRecord r, Resource resource) {
      this.r = r;
      this.resource = resource;
      this.attributes = OtelValues.attributes(r.attributes());
      String traceId = r.traceId().orElse(null);
      if (traceId == null) {
        this.spanContext = SpanContext.getInvalid();
      } else {
        byte flags = (byte) r.traceFlags().orElse(0);
        this.spanContext =
            SpanContext.create(
                traceId,
                r.spanId().orElse("0000000000000000"),
                TraceFlags.fromByte(flags),
                TraceState.getDefault());
      }
    }

    private static long nanos(Instant t) {
      return TimeUnit.SECONDS.toNanos(t.getEpochSecond()) + t.getNano();
    }

    @Override
    public Resource getResource() {
      return resource;
    }

    @Override
    public InstrumentationScopeInfo getInstrumentationScopeInfo() {
      return SCOPE;
    }

    @Override
    public long getTimestampEpochNanos() {
      return nanos(r.timestamp());
    }

    @Override
    public long getObservedTimestampEpochNanos() {
      return nanos(r.observedTimestamp().orElse(r.timestamp()));
    }

    @Override
    public SpanContext getSpanContext() {
      return spanContext;
    }

    @Override
    public Severity getSeverity() {
      int number = r.severity().number();
      for (Severity s : Severity.values()) {
        if (s.getSeverityNumber() == number) {
          return s;
        }
      }
      return Severity.UNDEFINED_SEVERITY_NUMBER;
    }

    @Override
    public String getSeverityText() {
      return r.severityText();
    }

    @Override
    @SuppressWarnings("deprecation")
    public Body getBody() {
      return Body.string(r.body());
    }

    @Override
    public Value<?> getBodyValue() {
      return Value.of(r.body());
    }

    @Override
    public Attributes getAttributes() {
      return attributes;
    }

    @Override
    public int getTotalAttributeCount() {
      return attributes.size();
    }
  }
}
