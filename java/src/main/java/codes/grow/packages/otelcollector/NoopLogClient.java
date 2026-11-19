package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.logging.BatchStats;
import codes.grow.packages.otelcollector.logging.LogClient;
import codes.grow.packages.otelcollector.logging.LogEntry;
import java.time.Duration;
import java.util.Collection;
import java.util.concurrent.CompletableFuture;

/** A log client that drops every record (logs disabled). */
final class NoopLogClient implements LogClient {
  static final NoopLogClient INSTANCE = new NoopLogClient();

  private NoopLogClient() {}

  @Override
  public void send(LogEntry entry) {}

  @Override
  public void sendBatch(Collection<? extends LogEntry> entries) {}

  @Override
  public void flush() {}

  @Override
  public void flush(Duration timeout) {}

  @Override
  public CompletableFuture<Void> flushAsync(Duration timeout) {
    return CompletableFuture.completedFuture(null);
  }

  @Override
  public void close() {}

  @Override
  public CompletableFuture<Void> closeAsync() {
    return CompletableFuture.completedFuture(null);
  }

  @Override
  public BatchStats stats() {
    return new BatchStats(0, 0, 0, 0, 0);
  }
}
