package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.logging.LogQueryClient;
import codes.grow.packages.otelcollector.logging.LogRecord;
import codes.grow.packages.otelcollector.logging.LogSearchResult;
import codes.grow.packages.otelcollector.logging.SearchOptions;
import codes.grow.packages.otelcollector.logging.WaitForLogOptions;

/**
 * {@link LogQueryClient} of the collector: the collector stores nothing and has no query API, so
 * every method throws {@link UnsupportedException}. Read logs from the backend it exports to.
 */
public final class CollectorLogQueryClient implements LogQueryClient {
  /** Creates the client. */
  public CollectorLogQueryClient() {}

  private static UnsupportedException unsupported(String op) {
    return new UnsupportedException(
        "logs."
            + op
            + ": the OpenTelemetry Collector has no query API (read logs from the backend it"
            + " exports to)");
  }

  /**
   * Always throws.
   *
   * @param query ignored
   * @param options ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public LogSearchResult search(String query, SearchOptions options) {
    throw unsupported("search");
  }

  /**
   * Always throws.
   *
   * @param query ignored
   * @param options ignored
   * @return never
   * @throws UnsupportedException always
   */
  @Override
  public LogRecord waitForLog(String query, WaitForLogOptions options) {
    throw unsupported("waitForLog");
  }
}
