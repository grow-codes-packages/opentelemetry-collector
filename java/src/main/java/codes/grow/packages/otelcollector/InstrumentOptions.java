package codes.grow.packages.otelcollector;

import java.util.List;
import java.util.Optional;

/**
 * Options of the metric instrument helpers ({@code counter}, {@code upDownCounter}, {@code
 * histogram}, {@code gauge}).
 *
 * @param description instrument description, may be null
 * @param unit UCUM unit such as {@code ms}, {@code By} or {@code {request}}, may be null
 * @param meter meter to create the instrument on, null for {@link
 *     OtelCollectorMetrics#DEFAULT_METER_NAME}
 * @param boundaries explicit histogram bucket boundaries (ascending, advice to the SDK), may be
 *     empty
 */
public record InstrumentOptions(
    String description, String unit, String meter, List<Double> boundaries) {
  /** No description, unit, meter or boundaries. */
  public static final InstrumentOptions NONE = new InstrumentOptions(null, null, null, List.of());

  /**
   * Copies the boundaries.
   *
   * @param description description
   * @param unit unit
   * @param meter meter name
   * @param boundaries boundaries
   */
  public InstrumentOptions {
    boundaries = boundaries == null ? List.of() : List.copyOf(boundaries);
  }

  /**
   * Options with a unit only.
   *
   * @param unit unit
   * @return the options
   */
  public static InstrumentOptions unit(String unit) {
    return new InstrumentOptions(null, unit, null, List.of());
  }

  /**
   * Histogram options with explicit bucket boundaries.
   *
   * @param boundaries ascending boundaries
   * @return the options
   */
  public static InstrumentOptions boundaries(Double... boundaries) {
    return new InstrumentOptions(null, null, null, List.of(boundaries));
  }

  /**
   * Returns a copy with a description.
   *
   * @param description description
   * @return the options
   */
  public InstrumentOptions withDescription(String description) {
    return new InstrumentOptions(description, unit, meter, boundaries);
  }

  /**
   * Returns a copy with a unit.
   *
   * @param unit unit
   * @return the options
   */
  public InstrumentOptions withUnit(String unit) {
    return new InstrumentOptions(description, unit, meter, boundaries);
  }

  /**
   * Returns a copy that creates the instrument on another meter.
   *
   * @param meter meter name
   * @return the options
   */
  public InstrumentOptions withMeter(String meter) {
    return new InstrumentOptions(description, unit, meter, boundaries);
  }

  /**
   * Returns a copy with histogram bucket boundaries.
   *
   * @param boundaries ascending boundaries
   * @return the options
   */
  public InstrumentOptions withBoundaries(List<Double> boundaries) {
    return new InstrumentOptions(description, unit, meter, boundaries);
  }

  Optional<String> descriptionOpt() {
    return Optional.ofNullable(description);
  }

  Optional<String> unitOpt() {
    return Optional.ofNullable(unit);
  }
}
