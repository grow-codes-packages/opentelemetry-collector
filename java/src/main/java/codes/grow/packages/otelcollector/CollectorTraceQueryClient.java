package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.tracing.query.DependencyLink;
import codes.grow.packages.otelcollector.tracing.query.Trace;
import codes.grow.packages.otelcollector.tracing.query.TraceQueryClient;
import codes.grow.packages.otelcollector.tracing.query.TraceSearchQuery;
import codes.grow.packages.otelcollector.tracing.query.TraceSummary;
import codes.grow.packages.otelcollector.tracing.query.WaitForTraceOptions;
import java.time.Instant;
import java.util.List;
import java.util.Optional;

/**
 * {@link TraceQueryClient} of the collector: the collector stores nothing and has no query API, so
 * every method throws {@link UnsupportedException}. Read traces from the backend it exports to.
 */
public final class CollectorTraceQueryClient implements TraceQueryClient {
  /** Creates the client. */
  public CollectorTraceQueryClient() {}

  private static UnsupportedException unsupported(String op) {
    return new UnsupportedException(
        "traces."
            + op
            + ": the OpenTelemetry Collector has no query API (read traces from the backend it"
            + " exports to)");
  }

  /**
   * Always throws.
   *
   * @param traceId ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public Optional<Trace> getTrace(String traceId) {
    throw unsupported("getTrace");
  }

  /**
   * Always throws.
   *
   * @param query ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public List<TraceSummary> searchTraces(TraceSearchQuery query) {
    throw unsupported("searchTraces");
  }

  /**
   * Always throws.
   *
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public List<String> listServices() {
    throw unsupported("listServices");
  }

  /**
   * Always throws.
   *
   * @param service ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public List<String> listOperations(String service) {
    throw unsupported("listOperations");
  }

  /**
   * Always throws.
   *
   * @param start ignored
   * @param end ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public List<DependencyLink> getDependencies(Instant start, Instant end) {
    throw unsupported("getDependencies");
  }

  /**
   * Always throws.
   *
   * @param traceId ignored
   * @param options ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public Trace waitForTrace(String traceId, WaitForTraceOptions options) {
    throw unsupported("waitForTrace");
  }
}
