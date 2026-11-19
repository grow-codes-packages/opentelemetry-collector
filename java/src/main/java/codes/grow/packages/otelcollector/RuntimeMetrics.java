package codes.grow.packages.otelcollector;

import io.opentelemetry.api.common.AttributeKey;
import io.opentelemetry.api.common.Attributes;
import io.opentelemetry.api.metrics.BatchCallback;
import io.opentelemetry.api.metrics.Meter;
import io.opentelemetry.api.metrics.ObservableDoubleMeasurement;
import io.opentelemetry.api.metrics.ObservableLongMeasurement;
import java.lang.management.ClassLoadingMXBean;
import java.lang.management.ManagementFactory;
import java.lang.management.MemoryMXBean;
import java.lang.management.OperatingSystemMXBean;
import java.lang.management.RuntimeMXBean;
import java.lang.management.ThreadMXBean;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * JVM runtime metrics (semantic-convention names) observed on every collection: {@code
 * jvm.memory.used} and {@code jvm.memory.committed} (By, {@code jvm.memory.type} heap / non_heap),
 * {@code jvm.thread.count}, {@code jvm.class.count}, {@code jvm.cpu.time} (s), {@code
 * jvm.cpu.recent_utilization} and {@code process.uptime} (s).
 */
public final class RuntimeMetrics implements AutoCloseable {
  private static final AttributeKey<String> MEMORY_TYPE = AttributeKey.stringKey("jvm.memory.type");
  private static final Attributes HEAP = Attributes.of(MEMORY_TYPE, "heap");
  private static final Attributes NON_HEAP = Attributes.of(MEMORY_TYPE, "non_heap");

  private final BatchCallback callback;
  private final AtomicBoolean closed = new AtomicBoolean();

  private RuntimeMetrics(BatchCallback callback) {
    this.callback = callback;
  }

  /**
   * Registers the runtime metrics on a meter.
   *
   * @param meter meter, usually named {@link OtelCollectorMetrics#RUNTIME_METER_NAME}
   * @return a handle; {@link #close()} stops the observation
   */
  public static RuntimeMetrics register(Meter meter) {
    MemoryMXBean memory = ManagementFactory.getMemoryMXBean();
    ThreadMXBean threads = ManagementFactory.getThreadMXBean();
    ClassLoadingMXBean classes = ManagementFactory.getClassLoadingMXBean();
    RuntimeMXBean runtime = ManagementFactory.getRuntimeMXBean();
    OperatingSystemMXBean os = ManagementFactory.getOperatingSystemMXBean();
    ObservableLongMeasurement used =
        meter
            .upDownCounterBuilder("jvm.memory.used")
            .setUnit("By")
            .setDescription("Measure of memory used.")
            .buildObserver();
    ObservableLongMeasurement committed =
        meter
            .upDownCounterBuilder("jvm.memory.committed")
            .setUnit("By")
            .setDescription("Measure of memory committed.")
            .buildObserver();
    ObservableLongMeasurement threadCount =
        meter
            .upDownCounterBuilder("jvm.thread.count")
            .setUnit("{thread}")
            .setDescription("Number of executing platform threads.")
            .buildObserver();
    ObservableLongMeasurement classCount =
        meter
            .upDownCounterBuilder("jvm.class.count")
            .setUnit("{class}")
            .setDescription("Number of classes currently loaded.")
            .buildObserver();
    ObservableDoubleMeasurement cpuTime =
        meter
            .counterBuilder("jvm.cpu.time")
            .ofDoubles()
            .setUnit("s")
            .setDescription("CPU time used by the process as reported by the JVM.")
            .buildObserver();
    ObservableDoubleMeasurement cpuUtilization =
        meter
            .gaugeBuilder("jvm.cpu.recent_utilization")
            .setUnit("1")
            .setDescription("Recent CPU utilization for the process as reported by the JVM.")
            .buildObserver();
    ObservableDoubleMeasurement uptime =
        meter
            .gaugeBuilder("process.uptime")
            .setUnit("s")
            .setDescription("The time the process has been running.")
            .buildObserver();
    BatchCallback cb =
        meter.batchCallback(
            () -> {
              used.record(memory.getHeapMemoryUsage().getUsed(), HEAP);
              used.record(memory.getNonHeapMemoryUsage().getUsed(), NON_HEAP);
              committed.record(memory.getHeapMemoryUsage().getCommitted(), HEAP);
              committed.record(memory.getNonHeapMemoryUsage().getCommitted(), NON_HEAP);
              threadCount.record(threads.getThreadCount());
              classCount.record(classes.getLoadedClassCount());
              if (os instanceof com.sun.management.OperatingSystemMXBean) {
                com.sun.management.OperatingSystemMXBean sun =
                    (com.sun.management.OperatingSystemMXBean) os;
                cpuTime.record(sun.getProcessCpuTime() / 1e9);
                double load = sun.getProcessCpuLoad();
                if (load >= 0) {
                  cpuUtilization.record(load);
                }
              }
              uptime.record(runtime.getUptime() / 1e3);
            },
            used,
            committed,
            threadCount,
            classCount,
            cpuTime,
            cpuUtilization,
            uptime);
    return new RuntimeMetrics(cb);
  }

  /** Stops the observation. Idempotent. */
  @Override
  public void close() {
    if (closed.compareAndSet(false, true)) {
      callback.close();
    }
  }
}
