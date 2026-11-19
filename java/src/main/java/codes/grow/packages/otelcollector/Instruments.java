package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.SdkInfo;
import io.opentelemetry.api.metrics.DoubleGauge;
import io.opentelemetry.api.metrics.DoubleGaugeBuilder;
import io.opentelemetry.api.metrics.DoubleHistogram;
import io.opentelemetry.api.metrics.DoubleHistogramBuilder;
import io.opentelemetry.api.metrics.LongCounter;
import io.opentelemetry.api.metrics.LongCounterBuilder;
import io.opentelemetry.api.metrics.LongUpDownCounter;
import io.opentelemetry.api.metrics.LongUpDownCounterBuilder;
import io.opentelemetry.api.metrics.Meter;
import io.opentelemetry.api.metrics.MeterProvider;
import java.util.function.Supplier;

/** Instrument helpers bound to a meter provider source. */
final class Instruments {
  private final Supplier<MeterProvider> provider;

  Instruments(Supplier<MeterProvider> provider) {
    this.provider = provider;
  }

  Meter meter(String name, String version) {
    String n = name == null || name.isEmpty() ? OtelCollectorMetrics.DEFAULT_METER_NAME : name;
    return provider
        .get()
        .meterBuilder(n)
        .setInstrumentationVersion(version == null ? SdkInfo.VERSION : version)
        .build();
  }

  private Meter meterFor(InstrumentOptions o) {
    return meter(o.meter(), null);
  }

  LongCounter counter(String name, InstrumentOptions o) {
    LongCounterBuilder b = meterFor(o).counterBuilder(name);
    o.descriptionOpt().ifPresent(b::setDescription);
    o.unitOpt().ifPresent(b::setUnit);
    return b.build();
  }

  LongUpDownCounter upDownCounter(String name, InstrumentOptions o) {
    LongUpDownCounterBuilder b = meterFor(o).upDownCounterBuilder(name);
    o.descriptionOpt().ifPresent(b::setDescription);
    o.unitOpt().ifPresent(b::setUnit);
    return b.build();
  }

  DoubleHistogram histogram(String name, InstrumentOptions o) {
    DoubleHistogramBuilder b = meterFor(o).histogramBuilder(name);
    o.descriptionOpt().ifPresent(b::setDescription);
    o.unitOpt().ifPresent(b::setUnit);
    if (!o.boundaries().isEmpty()) {
      b.setExplicitBucketBoundariesAdvice(o.boundaries());
    }
    return b.build();
  }

  DoubleGauge gauge(String name, InstrumentOptions o) {
    DoubleGaugeBuilder b = meterFor(o).gaugeBuilder(name);
    o.descriptionOpt().ifPresent(b::setDescription);
    o.unitOpt().ifPresent(b::setUnit);
    return b.build();
  }
}
