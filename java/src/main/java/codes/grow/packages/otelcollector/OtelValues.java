package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.logging.AttrValues;
import io.opentelemetry.api.common.AttributeKey;
import io.opentelemetry.api.common.Attributes;
import io.opentelemetry.api.common.AttributesBuilder;
import io.opentelemetry.api.common.Value;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Converts SDK attribute maps (strings, booleans, longs, doubles, lists and nested maps) to
 * OpenTelemetry {@link Attributes}: homogeneous primitive lists become array attributes, nested
 * maps and mixed lists become {@code AnyValue} ({@link AttributeKey#valueKey}) attributes.
 */
public final class OtelValues {
  private OtelValues() {}

  /**
   * Converts an attribute map.
   *
   * @param attributes attributes (normalized with {@link AttrValues#normalize})
   * @return OpenTelemetry attributes
   */
  public static Attributes attributes(Map<String, ?> attributes) {
    AttributesBuilder b = Attributes.builder();
    for (Map.Entry<String, ?> e : attributes.entrySet()) {
      if (e.getValue() == null) {
        continue;
      }
      put(b, e.getKey(), AttrValues.normalize(e.getValue()));
    }
    return b.build();
  }

  private static void put(AttributesBuilder b, String key, Object v) {
    if (v instanceof String) {
      b.put(AttributeKey.stringKey(key), (String) v);
    } else if (v instanceof Boolean) {
      b.put(AttributeKey.booleanKey(key), (Boolean) v);
    } else if (v instanceof Long) {
      b.put(AttributeKey.longKey(key), (Long) v);
    } else if (v instanceof Double) {
      b.put(AttributeKey.doubleKey(key), (Double) v);
    } else if (v instanceof List) {
      putList(b, key, (List<?>) v);
    } else {
      b.put(AttributeKey.valueKey(key), value(v));
    }
  }

  @SuppressWarnings("unchecked")
  private static void putList(AttributesBuilder b, String key, List<?> list) {
    Class<?> type = null;
    for (Object o : list) {
      Class<?> c = o.getClass();
      if (type == null) {
        type = c;
      } else if (type != c) {
        type = Object.class;
      }
    }
    if (type == String.class) {
      b.put(AttributeKey.stringArrayKey(key), (List<String>) list);
    } else if (type == Boolean.class) {
      b.put(AttributeKey.booleanArrayKey(key), (List<Boolean>) list);
    } else if (type == Long.class) {
      b.put(AttributeKey.longArrayKey(key), (List<Long>) list);
    } else if (type == Double.class) {
      b.put(AttributeKey.doubleArrayKey(key), (List<Double>) list);
    } else {
      b.put(AttributeKey.valueKey(key), value(list));
    }
  }

  /**
   * Converts one normalized value to an OpenTelemetry {@code AnyValue}.
   *
   * @param v value
   * @return the value
   */
  public static Value<?> value(Object v) {
    if (v instanceof String) {
      return Value.of((String) v);
    }
    if (v instanceof Boolean) {
      return Value.of((Boolean) v);
    }
    if (v instanceof Long) {
      return Value.of((Long) v);
    }
    if (v instanceof Double) {
      return Value.of((Double) v);
    }
    if (v instanceof List) {
      List<Value<?>> out = new ArrayList<>();
      for (Object o : (List<?>) v) {
        out.add(value(o));
      }
      return Value.of(out);
    }
    if (v instanceof Map) {
      Map<String, Value<?>> out = new LinkedHashMap<>();
      for (Map.Entry<?, ?> e : ((Map<?, ?>) v).entrySet()) {
        out.put(String.valueOf(e.getKey()), value(e.getValue()));
      }
      return Value.of(out);
    }
    return Value.of(String.valueOf(v));
  }
}
