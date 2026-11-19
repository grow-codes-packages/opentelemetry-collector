package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.CancelledException;
import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.HealthStatus;
import codes.grow.packages.otelcollector.core.HttpTransport;
import codes.grow.packages.otelcollector.core.OtelCollectorException;
import codes.grow.packages.otelcollector.core.Poll;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.TransportRequest;
import codes.grow.packages.otelcollector.core.TransportResponse;
import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.core.ValidationException;
import com.fasterxml.jackson.databind.JsonNode;
import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Health of an OpenTelemetry Collector: {@code GET {healthUrl}} ({@code health_check} extension,
 * port 13133) and, when that endpoint is unreachable (stock Grow stacks bind it to localhost),
 * {@code GET {zpagesUrl}/debug/servicez} (port 55679). Also reads the collector version.
 *
 * <p>Admin endpoints are unauthenticated: the configured export headers are never sent to them.
 * Thread-safe; close it when done (a health client obtained from {@link
 * OtelCollectorClient#collectorHealth()} is closed with the client).
 */
public final class CollectorHealthClient implements AutoCloseable {
  /** Default deadline of {@link #waitUntilReady()}. */
  public static final Duration DEFAULT_READY_TIMEOUT = Duration.ofSeconds(30);

  /** Pause between probes of {@link #waitUntilReady()}. */
  public static final Duration DEFAULT_READY_INTERVAL = Duration.ofMillis(500);

  private static final Pattern VERSION_ROW =
      Pattern.compile("<b>Version</b></td>\\s*<td>[^<]*</td>\\s*<td[^>]*>([^<]+)</td>");
  private static final Pattern TARGET_INFO =
      Pattern.compile("(?m)^target_info\\{[^}]*\\bservice_version=\"([^\"]+)\"");

  private final OtelCollectorConfig config;
  private final HttpTransport transport;
  private final Duration interval;

  CollectorHealthClient(OtelCollectorConfig config, Duration interval) {
    this.config = config;
    this.transport = new HttpTransport(config.adminTransportConfig());
    this.interval = interval;
  }

  /**
   * Creates a health client.
   *
   * @param config validated configuration
   * @return a new client
   */
  public static CollectorHealthClient create(OtelCollectorConfig config) {
    config.validate();
    return new CollectorHealthClient(config, DEFAULT_READY_INTERVAL);
  }

  /**
   * Creates a health client from the environment (see {@link OtelCollectorConfig#fromEnv()}).
   *
   * @return a new client
   */
  public static CollectorHealthClient fromEnv() {
    return create(OtelCollectorConfig.fromEnv());
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
   * Parses the collector version from the zpages {@code /debug/servicez} HTML.
   *
   * @param html page
   * @return the version, or empty
   */
  public static Optional<String> parseServicezVersion(String html) {
    Matcher m = VERSION_ROW.matcher(html);
    return m.find() ? Optional.of(m.group(1).trim()).filter(s -> !s.isEmpty()) : Optional.empty();
  }

  /**
   * Parses {@code service_version} of the Prometheus {@code target_info} series.
   *
   * @param metrics exposition text
   * @return the version, or empty
   */
  public static Optional<String> parseTargetInfoVersion(String metrics) {
    Matcher m = TARGET_INFO.matcher(metrics);
    return m.find() ? Optional.of(m.group(1)) : Optional.empty();
  }

  /**
   * Collector health. {@code 200 {"status":"Server available",…}} → {@code healthy=true,
   * status="ok"} with details {@code source=health_check}, {@code vendorStatus}, {@code upSince},
   * {@code uptime}; another status ({@code 503} while starting or stopping) → {@code unhealthy};
   * unreachable → zpages fallback ({@code details.source=zpages}), else {@code unreachable}. Never
   * throws for an unhealthy or unreachable collector.
   *
   * @return the status
   * @throws CancelledException when interrupted or closed
   */
  public HealthStatus health() {
    return HttpTransport.await(healthAsync(), "health");
  }

  /**
   * Asynchronous {@link #health()}.
   *
   * @return a future with the status (failed only for cancellation and configuration errors)
   */
  public CompletableFuture<HealthStatus> healthAsync() {
    return probe(config.healthUrl())
        .thenCompose(
            primary -> {
              if (primary.response() != null) {
                return CompletableFuture.completedFuture(fromHealthResponse(primary.response()));
              }
              String reason = String.valueOf(primary.error().getMessage());
              return probe(zpages("/debug/servicez"))
                  .thenApply(
                      z -> {
                        if (z.response() != null && z.response().status() / 100 == 2) {
                          Map<String, String> d = new LinkedHashMap<>();
                          d.put("source", "zpages");
                          d.put("httpStatus", Integer.toString(z.response().status()));
                          d.put("healthCheckError", reason);
                          return new HealthStatus(true, "ok", d);
                        }
                        return HealthStatus.unreachable(reason);
                      });
            });
  }

  private record Probe(TransportResponse response, OtelCollectorException error) {}

  private CompletableFuture<Probe> probe(String url) {
    return transport
        .sendAsync(
            TransportRequest.get(url).operation("health").acceptAnyStatus().maxRetries(0).build())
        .handle(
            (resp, err) -> {
              if (err == null) {
                return new Probe(resp, null);
              }
              OtelCollectorException e = HttpTransport.asSdkException(err, "health");
              if (e instanceof CancelledException || e instanceof ConfigException) {
                throw e;
              }
              return new Probe(null, e);
            });
  }

  private static HealthStatus fromHealthResponse(TransportResponse resp) {
    boolean ok = resp.status() / 100 == 2;
    Map<String, String> details = new LinkedHashMap<>();
    details.put("source", "health_check");
    try {
      JsonNode body = resp.jsonNode();
      if (body != null && body.isObject()) {
        for (Map.Entry<String, JsonNode> e : body.properties()) {
          if (e.getValue().isValueNode()) {
            String key = e.getKey().equals("status") ? "vendorStatus" : e.getKey();
            details.put(key, e.getValue().asText());
          }
        }
      }
    } catch (ValidationException ignored) {
      // not JSON: no vendor details
    }
    if (!ok) {
      details.put("httpStatus", Integer.toString(resp.status()));
    }
    return new HealthStatus(ok, ok ? "ok" : "unhealthy", details);
  }

  /**
   * Polls {@link #health()} until the collector is healthy, for at most {@link
   * #DEFAULT_READY_TIMEOUT}.
   *
   * @return the healthy status
   * @throws TimeoutException at the deadline
   * @throws CancelledException when interrupted
   */
  public HealthStatus waitUntilReady() {
    return waitUntilReady(DEFAULT_READY_TIMEOUT);
  }

  /**
   * Polls {@link #health()} until the collector is healthy.
   *
   * @param timeout deadline
   * @return the healthy status
   * @throws TimeoutException at the deadline
   * @throws CancelledException when interrupted
   */
  public HealthStatus waitUntilReady(Duration timeout) {
    return Poll.until(
        "health.waitUntilReady",
        timeout,
        interval,
        () -> Optional.of(health()).filter(HealthStatus::healthy));
  }

  /**
   * Asynchronous {@link #waitUntilReady(Duration)}: no thread blocks between probes.
   *
   * @param timeout deadline
   * @return a future with the healthy status, failed with {@link TimeoutException}
   */
  public CompletableFuture<HealthStatus> waitUntilReadyAsync(Duration timeout) {
    return Poll.untilAsync(
        "health.waitUntilReady",
        timeout,
        interval,
        () -> healthAsync().thenApply(h -> Optional.of(h).filter(HealthStatus::healthy)));
  }

  /**
   * Collector version: the zpages {@code /debug/servicez} "Version" row (works on stock stacks),
   * else {@code service_version} of {@code target_info} at {@link
   * OtelCollectorConfig#telemetryUrl()}.
   *
   * @return the version, such as {@code 0.165.0}
   * @throws UnsupportedException when neither source answers with a version
   */
  public String serverVersion() {
    return HttpTransport.await(serverVersionAsync(), "serverVersion");
  }

  /**
   * Asynchronous {@link #serverVersion()}.
   *
   * @return a future with the version
   */
  public CompletableFuture<String> serverVersionAsync() {
    return text(zpages("/debug/servicez"))
        .thenCompose(
            page -> {
              Optional<String> v = page.flatMap(CollectorHealthClient::parseServicezVersion);
              if (v.isPresent()) {
                return CompletableFuture.completedFuture(v);
              }
              if (config.telemetryUrl().isEmpty()) {
                return CompletableFuture.completedFuture(Optional.<String>empty());
              }
              return text(config.telemetryUrl().get())
                  .thenApply(m -> m.flatMap(CollectorHealthClient::parseTargetInfoVersion));
            })
        .thenApply(
            v ->
                v.orElseThrow(
                    () ->
                        new UnsupportedException(
                            "serverVersion: the collector exposes no version (zpages and internal"
                                + " telemetry unavailable)")));
  }

  /** GET returning the body text on 2xx, empty on any other outcome except cancellation. */
  private CompletableFuture<Optional<String>> text(String url) {
    return transport
        .sendAsync(
            TransportRequest.get(url)
                .operation("serverVersion")
                .header("Accept", "text/html, text/plain")
                .acceptAnyStatus()
                .build())
        .handle(
            (resp, err) -> {
              if (err != null) {
                OtelCollectorException e = HttpTransport.asSdkException(err, "serverVersion");
                if (e instanceof CancelledException || e instanceof ConfigException) {
                  throw e;
                }
                return Optional.empty();
              }
              return resp.status() / 100 == 2 ? Optional.of(resp.text()) : Optional.empty();
            });
  }

  private String zpages(String path) {
    return config.zpagesUrl().replaceAll("/+$", "") + path;
  }

  /** Releases connections. Idempotent. */
  @Override
  public void close() {
    transport.close();
  }
}
