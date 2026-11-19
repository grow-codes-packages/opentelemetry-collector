package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.Json;
import codes.grow.packages.otelcollector.core.MockServer;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import com.fasterxml.jackson.databind.JsonNode;
import com.sun.net.httpserver.HttpExchange;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.HexFormat;
import java.util.Map;
import mockwebserver3.MockResponse;
import mockwebserver3.MockWebServer;
import okhttp3.Headers;
import okhttp3.Protocol;
import okio.Buffer;

/** Fixtures and mock helpers shared by the service tests. */
final class TestSupport {
  // JUL keeps loggers weakly: hold a strong reference or the OFF level is lost after a GC.
  private static final java.util.logging.Logger OTEL_JUL =
      java.util.logging.Logger.getLogger("io.opentelemetry");

  private TestSupport() {}

  static final RetryPolicy FAST_RETRY =
      RetryPolicy.builder()
          .maxRetries(2)
          .initialBackoff(Duration.ofMillis(1))
          .maxBackoff(Duration.ofMillis(5))
          .build();

  /**
   * Silences the OpenTelemetry SDK's own JUL warnings (expected export failures). Under surefire
   * (TZ=UTC) the first JUL record written from an OkHttp thread can fail to initialise.
   */
  static void quietOtelLogs() {
    OTEL_JUL.setLevel(java.util.logging.Level.OFF);
  }

  /** A recorded fixture from {@code src/test/resources/fixtures}. */
  static JsonNode fixture(String name) {
    try (InputStream in = TestSupport.class.getResourceAsStream("/fixtures/" + name + ".json")) {
      if (in == null) {
        throw new IllegalArgumentException("no fixture " + name);
      }
      return Json.tree(in.readAllBytes());
    } catch (IOException e) {
      throw new UncheckedIOException(e);
    }
  }

  /** Response body bytes of a fixture (hex, base64, text or json). */
  static byte[] body(String name) {
    JsonNode resp = fixture(name).path("response");
    if (resp.has("bodyExcerpt")) {
      StringBuilder sb = new StringBuilder();
      resp.get("bodyExcerpt").forEach(l -> sb.append(l.asText()).append('\n'));
      return sb.toString().getBytes(StandardCharsets.UTF_8);
    }
    JsonNode b = resp.path("body");
    if (b.has("hex")) {
      return HexFormat.of().parseHex(b.get("hex").asText());
    }
    if (b.has("text")) {
      return b.get("text").asText().getBytes(StandardCharsets.UTF_8);
    }
    if (b.has("json")) {
      return b.get("json").toString().getBytes(StandardCharsets.UTF_8);
    }
    return new byte[0];
  }

  static int status(String name) {
    return fixture(name).path("response").path("status").asInt();
  }

  static String contentType(String name) {
    return fixture(name).path("response").path("headers").path("Content-Type").asText("");
  }

  /** Responder that replays a fixture's status, content type and body bytes. */
  static MockServer.Responder replay(String name) {
    return ex -> send(ex, status(name), contentType(name), body(name));
  }

  static void send(HttpExchange ex, int status, String contentType, byte[] body)
      throws IOException {
    if (!contentType.isEmpty()) {
      ex.getResponseHeaders().add("Content-Type", contentType);
    }
    ex.sendResponseHeaders(status, body.length == 0 ? -1 : body.length);
    if (body.length > 0) {
      try (OutputStream os = ex.getResponseBody()) {
        os.write(body);
      }
    }
  }

  static MockServer.Responder bytes(int status, String hex) {
    return ex -> send(ex, status, "application/x-protobuf", HexFormat.of().parseHex(hex));
  }

  /** A gRPC (h2c) mock server. */
  static MockWebServer grpcServer() throws IOException {
    MockWebServer s = new MockWebServer();
    s.setProtocols(java.util.List.of(Protocol.H2_PRIOR_KNOWLEDGE));
    s.start();
    return s;
  }

  static String grpcUrl(MockWebServer s) {
    return "http://127.0.0.1:" + s.url("/").port();
  }

  /** gRPC response with a message (hex, may be empty) and status trailers. */
  static MockResponse grpc(int status, String message, String messageHex) {
    byte[] msg = HexFormat.of().parseHex(messageHex);
    Buffer frame = new Buffer();
    frame.writeByte(0);
    frame.writeInt(msg.length);
    frame.write(msg);
    Headers.Builder trailers = new Headers.Builder().add("grpc-status", Integer.toString(status));
    if (message != null) {
      trailers.add("grpc-message", message);
    }
    return new MockResponse.Builder()
        .code(200)
        .addHeader("content-type", "application/grpc")
        .body(frame)
        .trailers(trailers.build())
        .build();
  }

  /** A self-signed certificate (PEM) made with the JDK keytool. */
  static String selfSignedPem(java.nio.file.Path dir) throws Exception {
    java.nio.file.Path store = dir.resolve("ca.p12");
    String keytool =
        java.nio.file.Paths.get(System.getProperty("java.home"), "bin", "keytool").toString();
    run(
        keytool,
        "-genkeypair",
        "-alias",
        "ca",
        "-keyalg",
        "EC",
        "-dname",
        "CN=localhost",
        "-validity",
        "2",
        "-keystore",
        store.toString(),
        "-storetype",
        "PKCS12",
        "-storepass",
        "changeit",
        "-keypass",
        "changeit");
    return run(
        keytool,
        "-exportcert",
        "-rfc",
        "-alias",
        "ca",
        "-keystore",
        store.toString(),
        "-storepass",
        "changeit");
  }

  private static String run(String... cmd) throws Exception {
    Process p = new ProcessBuilder(cmd).redirectErrorStream(true).start();
    String out = new String(p.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
    if (!p.waitFor(60, java.util.concurrent.TimeUnit.SECONDS) || p.exitValue() != 0) {
      throw new IllegalStateException("keytool failed: " + out);
    }
    return out.substring(Math.max(0, out.indexOf("-----BEGIN")));
  }

  /** A local port nothing listens on. */
  static int closedPort() {
    try (ServerSocket s = new ServerSocket(0)) {
      return s.getLocalPort();
    } catch (IOException e) {
      throw new UncheckedIOException(e);
    }
  }

  static Map<String, String> env(String... kv) {
    java.util.Map<String, String> m = new java.util.LinkedHashMap<>();
    for (int i = 0; i + 1 < kv.length; i += 2) {
      m.put(kv[i], kv[i + 1]);
    }
    return m;
  }
}
