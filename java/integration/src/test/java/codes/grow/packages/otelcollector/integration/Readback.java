package codes.grow.packages.otelcollector.integration;

import codes.grow.packages.otelcollector.core.Json;
import codes.grow.packages.otelcollector.core.UnavailableException;
import codes.grow.packages.otelcollector.core.UnsupportedException;
import codes.grow.packages.otelcollector.logging.LogQueryClient;
import codes.grow.packages.otelcollector.logging.LogRecord;
import codes.grow.packages.otelcollector.logging.LogSearchResult;
import codes.grow.packages.otelcollector.logging.SearchOptions;
import codes.grow.packages.otelcollector.logging.Severity;
import codes.grow.packages.otelcollector.tracing.query.Span;
import codes.grow.packages.otelcollector.tracing.query.Trace;
import codes.grow.packages.otelcollector.tracing.query.TraceQueryClient;
import codes.grow.packages.otelcollector.tracing.query.TraceSearchQuery;
import codes.grow.packages.otelcollector.tracing.query.TraceSummary;
import codes.grow.packages.otelcollector.tracing.query.Traces;
import com.fasterxml.jackson.databind.JsonNode;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;

/**
 * Test-only read-back adapters (DECISIONS.md section 7): the collector stores nothing, so traces
 * are read from the Jaeger sidecar ({@code OTELCOL_TEST_JAEGER_URL}, {@code /api/v3}) and logs and
 * metrics from the file exporter output served by the capture sidecar ({@code
 * OTELCOL_TEST_CAPTURE_URL/<signal>.jsonl}). They are not shipped in the package.
 */
final class Readback {
  private static final HttpClient HTTP =
      HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();

  private Readback() {}

  static Map<String, String> env() {
    try {
      return ConformanceTest.environment();
    } catch (IOException e) {
      throw new UncheckedIOException(e);
    }
  }

  /** GET returning status and body. */
  static HttpResponse<String> get(String url) {
    try {
      return HTTP.send(
          HttpRequest.newBuilder(URI.create(url)).timeout(Duration.ofSeconds(10)).GET().build(),
          HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8));
    } catch (IOException e) {
      throw new UnavailableException("readback: " + e.getMessage(), e);
    } catch (InterruptedException e) {
      Thread.currentThread().interrupt();
      throw new UnavailableException("readback: interrupted", e);
    }
  }

  /** The JSON lines of one capture file ({@code traces}, {@code logs}, {@code metrics}). */
  static List<JsonNode> captureLines(String signal) {
    HttpResponse<String> r = get(env().get("OTELCOL_TEST_CAPTURE_URL") + "/" + signal + ".jsonl");
    List<JsonNode> out = new ArrayList<>();
    if (r.statusCode() == 404) {
      return out;
    }
    if (r.statusCode() != 200) {
      throw new UnavailableException("capture " + signal + ": HTTP " + r.statusCode());
    }
    for (String line : r.body().split("\n")) {
      if (!line.isBlank()) {
        out.add(Json.tree(line.getBytes(StandardCharsets.UTF_8)));
      }
    }
    return out;
  }

  static String captureText(String signal) {
    HttpResponse<String> r = get(env().get("OTELCOL_TEST_CAPTURE_URL") + "/" + signal + ".jsonl");
    return r.statusCode() == 200 ? r.body() : "";
  }

  static Object anyValue(JsonNode v) {
    if (v == null || v.isNull()) {
      return null;
    }
    if (v.has("stringValue")) {
      return v.get("stringValue").asText();
    }
    if (v.has("boolValue")) {
      return v.get("boolValue").asBoolean();
    }
    if (v.has("intValue")) {
      return Long.parseLong(v.get("intValue").asText());
    }
    if (v.has("doubleValue")) {
      return v.get("doubleValue").asDouble();
    }
    if (v.has("arrayValue")) {
      List<Object> out = new ArrayList<>();
      v.get("arrayValue").path("values").forEach(x -> out.add(anyValue(x)));
      return out;
    }
    if (v.has("kvlistValue")) {
      return kv(v.get("kvlistValue").path("values"));
    }
    return null;
  }

  static Map<String, Object> kv(JsonNode list) {
    Map<String, Object> out = new LinkedHashMap<>();
    if (list != null) {
      list.forEach(e -> out.put(e.path("key").asText(), anyValue(e.get("value"))));
    }
    return out;
  }

  private static Instant nanos(JsonNode n) {
    if (n == null || n.isNull()) {
      return Instant.EPOCH;
    }
    long v = Long.parseLong(n.asText());
    return Instant.ofEpochSecond(0, v);
  }

  /** Every log record of logs.jsonl. */
  static List<LogRecord> captureLogs() {
    List<LogRecord> out = new ArrayList<>();
    for (JsonNode line : captureLines("logs")) {
      for (JsonNode rl : line.path("resourceLogs")) {
        Map<String, Object> resource = kv(rl.path("resource").path("attributes"));
        for (JsonNode sl : rl.path("scopeLogs")) {
          for (JsonNode lr : sl.path("logRecords")) {
            Object body = anyValue(lr.get("body"));
            LogRecord.Builder b =
                LogRecord.builder()
                    .timestamp(nanos(lr.get("timeUnixNano")))
                    .severity(Severity.fromNumber(lr.path("severityNumber").asInt(9)))
                    .severityText(lr.path("severityText").asText(""))
                    .body(body == null ? "" : String.valueOf(body))
                    .attributes(kv(lr.path("attributes")))
                    .resource(resource);
            String traceId = lr.path("traceId").asText("");
            if (!traceId.isEmpty()) {
              b.traceId(traceId);
            }
            String spanId = lr.path("spanId").asText("");
            if (!spanId.isEmpty()) {
              b.spanId(spanId);
            }
            if (lr.has("flags")) {
              b.traceFlags(lr.get("flags").asInt() & 0xff);
            }
            out.add(b.build());
          }
        }
      }
    }
    return out;
  }

  /** Conformance log query: substring match on the record body in logs.jsonl. */
  static final class CaptureLogQuery implements LogQueryClient {
    @Override
    public LogSearchResult search(String query, SearchOptions options) {
      List<LogRecord> hits = new ArrayList<>();
      for (LogRecord r : captureLogs()) {
        if (r.body().contains(query) && hits.size() < options.limit()) {
          hits.add(r);
        }
      }
      return LogSearchResult.of(hits);
    }
  }

  /** Conformance trace query on the Jaeger v3 API. */
  static final class JaegerTraceQuery implements TraceQueryClient {
    private static final Span.Kind[] KINDS = Span.Kind.values();
    private static final Span.StatusCode[] STATUS = Span.StatusCode.values();

    @Override
    public Optional<Trace> getTrace(String traceId) {
      HttpResponse<String> r = get(env().get("OTELCOL_TEST_JAEGER_URL") + "/api/v3/traces/" + traceId);
      if (r.statusCode() == 404) {
        return Optional.empty();
      }
      if (r.statusCode() != 200) {
        throw new UnavailableException("jaeger getTrace: HTTP " + r.statusCode());
      }
      JsonNode doc = Json.tree(r.body().getBytes(StandardCharsets.UTF_8));
      List<Span> spans = new ArrayList<>();
      for (JsonNode rs : doc.path("result").path("resourceSpans")) {
        Object svc = kv(rs.path("resource").path("attributes")).get("service.name");
        for (JsonNode ss : rs.path("scopeSpans")) {
          for (JsonNode s : ss.path("spans")) {
            Instant start = nanos(s.get("startTimeUnixNano"));
            Instant end = nanos(s.get("endTimeUnixNano"));
            long startMicros = start.getEpochSecond() * 1_000_000L + start.getNano() / 1000;
            long endMicros = end.getEpochSecond() * 1_000_000L + end.getNano() / 1000;
            Span.Builder b =
                Span.builder(s.path("traceId").asText(), s.path("spanId").asText(), s.path("name").asText())
                    .serviceName(svc == null ? "" : String.valueOf(svc))
                    .times(startMicros, endMicros)
                    .attributes(kv(s.path("attributes")));
            String parent = s.path("parentSpanId").asText("");
            if (!parent.isEmpty()) {
              b.parentSpanId(parent);
            }
            int kind = s.path("kind").asInt(0);
            b.kind(kind >= 0 && kind < KINDS.length ? KINDS[kind] : Span.Kind.UNSPECIFIED);
            int code = s.path("status").path("code").asInt(0);
            String msg = s.path("status").path("message").asText(null);
            b.status(code >= 0 && code < STATUS.length ? STATUS[code] : Span.StatusCode.UNSET, msg);
            for (JsonNode ev : s.path("events")) {
              Instant t = nanos(ev.get("timeUnixNano"));
              b.event(
                  new Span.Event(
                      ev.path("name").asText(),
                      t.getEpochSecond() * 1_000_000L + t.getNano() / 1000,
                      kv(ev.path("attributes"))));
            }
            spans.add(b.build());
          }
        }
      }
      return Optional.of(new Trace(traceId, Traces.sortSpans(spans)));
    }

    @Override
    public List<TraceSummary> searchTraces(TraceSearchQuery query) {
      throw new UnsupportedException("jaeger adapter: searchTraces is not used by the tests");
    }

    @Override
    public List<String> listServices() {
      HttpResponse<String> r = get(env().get("OTELCOL_TEST_JAEGER_URL") + "/api/v3/services");
      if (r.statusCode() != 200) {
        throw new UnavailableException("jaeger services: HTTP " + r.statusCode());
      }
      List<String> out = new ArrayList<>();
      Json.tree(r.body().getBytes(StandardCharsets.UTF_8))
          .path("services")
          .forEach(s -> out.add(s.asText()));
      out.sort(null);
      return out;
    }

    @Override
    public List<String> listOperations(String service) {
      throw new UnsupportedException("jaeger adapter: listOperations is not used by the tests");
    }
  }
}
