package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.ErrorCode;
import codes.grow.packages.otelcollector.core.HttpTransport;
import codes.grow.packages.otelcollector.core.Json;
import codes.grow.packages.otelcollector.core.OtelCollectorException;
import codes.grow.packages.otelcollector.core.Redaction;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.core.UnavailableException;
import com.fasterxml.jackson.databind.JsonNode;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InterruptedIOException;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.security.cert.Certificate;
import java.security.cert.CertificateFactory;
import java.security.cert.X509Certificate;
import java.util.Collection;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.function.BiConsumer;
import java.util.function.Consumer;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.TrustManagerFactory;
import javax.net.ssl.X509TrustManager;

/**
 * Shared OTLP plumbing: gRPC method paths, decoding of {@code google.rpc.Status} and partial
 * success bodies, mapping of HTTP and gRPC statuses to the SDK exceptions, and exporter settings
 * (retry policy, TLS) for the official OpenTelemetry exporters.
 */
public final class Otlp {
  /** gRPC method of {@code TraceService/Export}. */
  public static final String GRPC_TRACES_METHOD =
      "opentelemetry.proto.collector.trace.v1.TraceService/Export";

  /** gRPC method of {@code LogsService/Export}. */
  public static final String GRPC_LOGS_METHOD =
      "opentelemetry.proto.collector.logs.v1.LogsService/Export";

  /** gRPC method of {@code MetricsService/Export}. */
  public static final String GRPC_METRICS_METHOD =
      "opentelemetry.proto.collector.metrics.v1.MetricsService/Export";

  private static final String[] GRPC_NAMES = {
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED"
  };

  private Otlp() {}

  /**
   * A decoded {@code google.rpc.Status}.
   *
   * @param code gRPC status code (field 1)
   * @param message message (field 2), may be empty
   */
  public record RpcStatus(int code, String message) {}

  /**
   * A decoded OTLP partial success.
   *
   * @param rejected rejected spans, log records or data points (field 1)
   * @param message error message (field 2), may be empty
   */
  public record PartialSuccess(long rejected, String message) {
    /** No partial success (everything accepted). */
    public static final PartialSuccess NONE = new PartialSuccess(0, "");
  }

  /** Minimal protobuf reader: top-level varint and length-delimited fields. */
  private static final class Fields {
    final Map<Integer, Long> varints = new java.util.HashMap<>();
    final Map<Integer, byte[]> bytes = new java.util.HashMap<>();

    static Optional<Fields> parse(byte[] b) {
      Fields f = new Fields();
      int[] pos = {0};
      while (pos[0] < b.length) {
        long tag = varint(b, pos);
        if (pos[0] < 0) {
          return Optional.empty();
        }
        int field = (int) (tag >>> 3);
        int wire = (int) (tag & 7);
        if (wire == 0) {
          long v = varint(b, pos);
          if (pos[0] < 0) {
            return Optional.empty();
          }
          f.varints.putIfAbsent(field, v);
        } else if (wire == 2) {
          long len = varint(b, pos);
          if (pos[0] < 0 || len < 0 || pos[0] + len > b.length) {
            return Optional.empty();
          }
          byte[] v = java.util.Arrays.copyOfRange(b, pos[0], pos[0] + (int) len);
          f.bytes.putIfAbsent(field, v);
          pos[0] += (int) len;
        } else if (wire == 5) {
          pos[0] += 4;
        } else if (wire == 1) {
          pos[0] += 8;
        } else {
          return Optional.empty();
        }
        if (pos[0] > b.length) {
          return Optional.empty();
        }
      }
      return Optional.of(f);
    }

    /** Reads a varint; returns -1 and sets pos to -1 when truncated or too long. */
    private static long varint(byte[] b, int[] pos) {
      long value = 0;
      for (int shift = 0; shift < 64; shift += 7) {
        if (pos[0] >= b.length) {
          pos[0] = -1;
          return -1;
        }
        byte x = b[pos[0]++];
        value |= (long) (x & 0x7f) << shift;
        if ((x & 0x80) == 0) {
          return value;
        }
      }
      pos[0] = -1;
      return -1;
    }
  }

  /**
   * Decodes a binary {@code google.rpc.Status} (field 1 {@code code}, field 2 {@code message}).
   *
   * @param body bytes
   * @return the status, or empty when the bytes are not a well-formed non-empty Status
   */
  public static Optional<RpcStatus> decodeRpcStatus(byte[] body) {
    if (body == null || body.length == 0) {
      return Optional.empty();
    }
    return Fields.parse(body)
        .filter(f -> !f.varints.isEmpty() || !f.bytes.isEmpty())
        .map(
            f ->
                new RpcStatus(
                    f.varints.getOrDefault(1, 0L).intValue(),
                    new String(f.bytes.getOrDefault(2, new byte[0]), StandardCharsets.UTF_8)));
  }

  /**
   * Decodes the {@code partial_success} (field 1) of a protobuf {@code Export*ServiceResponse}.
   *
   * @param body response bytes
   * @return the partial success, {@link PartialSuccess#NONE} when absent or malformed
   */
  public static PartialSuccess decodePartialSuccess(byte[] body) {
    if (body == null || body.length == 0) {
      return PartialSuccess.NONE;
    }
    Optional<byte[]> ps = Fields.parse(body).map(f -> f.bytes.get(1));
    if (ps.isEmpty() || ps.get() == null) {
      return PartialSuccess.NONE;
    }
    return Fields.parse(ps.get())
        .map(
            f ->
                new PartialSuccess(
                    Math.max(0, f.varints.getOrDefault(1, 0L)),
                    new String(f.bytes.getOrDefault(2, new byte[0]), StandardCharsets.UTF_8)))
        .orElse(PartialSuccess.NONE);
  }

  /**
   * Human text of an OTLP error body: JSON {@code {"code","message"}}, a binary {@code
   * google.rpc.Status}, or printable text. Whitespace collapsed; not yet redacted.
   *
   * @param body response body
   * @return the text, or empty
   */
  public static Optional<String> errorText(byte[] body) {
    if (body == null || body.length == 0) {
      return Optional.empty();
    }
    String text = new String(body, StandardCharsets.UTF_8).trim();
    String message = null;
    if (text.startsWith("{")) {
      try {
        JsonNode node = Json.tree(body);
        JsonNode m = node.get("message");
        message = m != null && m.isTextual() ? m.asText() : null;
      } catch (OtelCollectorException e) {
        message = null;
      }
    }
    if (message == null) {
      Optional<RpcStatus> st = decodeRpcStatus(body);
      if (st.isPresent() && !st.get().message().isEmpty()) {
        message = st.get().message();
      } else if (text.chars().allMatch(c -> c >= 0x20 && c < 0x7f || Character.isWhitespace(c))) {
        message = text;
      }
    }
    if (message == null) {
      return Optional.empty();
    }
    String clean = message.replaceAll("\\s+", " ").trim();
    return clean.isEmpty() ? Optional.empty() : Optional.of(clean);
  }

  /**
   * Builds the SDK exception for an HTTP status with an OTLP error body: {@code "<op>: <phrase>:
   * <message> (HTTP <status>)"}, redacted and truncated.
   *
   * @param operation operation name
   * @param status HTTP status
   * @param body response body (protobuf Status, JSON or text), may be null
   * @param secrets secret values to remove
   * @return the exception of the matching subclass
   */
  public static OtelCollectorException httpError(
      String operation, int status, byte[] body, Collection<String> secrets) {
    byte[] text = errorText(body).map(t -> t.getBytes(StandardCharsets.UTF_8)).orElse(null);
    String message =
        HttpTransport.errorMessage(
            operation, status, Map.of("Content-Type", List.of("text/plain")), text, secrets);
    return OtelCollectorException.of(
        ErrorCode.forStatus(status), message, status, null, null, null);
  }

  /**
   * Maps a gRPC status code to an SDK {@link ErrorCode}.
   *
   * @param status gRPC status code
   * @return the code
   */
  public static ErrorCode codeForGrpcStatus(int status) {
    switch (status) {
      case 1:
        return ErrorCode.CANCELLED;
      case 3:
      case 9:
      case 11:
      case 13:
        return ErrorCode.VALIDATION;
      case 4:
        return ErrorCode.TIMEOUT;
      case 5:
      case 12:
        return ErrorCode.NOT_FOUND;
      case 6:
      case 10:
        return ErrorCode.CONFLICT;
      case 7:
        return ErrorCode.PERMISSION_DENIED;
      case 8:
        return ErrorCode.RATE_LIMITED;
      case 16:
        return ErrorCode.AUTHENTICATION;
      default:
        return ErrorCode.UNAVAILABLE;
    }
  }

  /**
   * Name of a gRPC status code ({@code UNAUTHENTICATED}, …).
   *
   * @param status code
   * @return the name, or the number for unknown codes
   */
  public static String grpcStatusName(int status) {
    return status >= 0 && status < GRPC_NAMES.length
        ? GRPC_NAMES[status]
        : Integer.toString(status);
  }

  /**
   * Builds the SDK exception for a gRPC status: {@code "<op>: <message> (gRPC <NAME>)"}, redacted
   * and truncated.
   *
   * @param operation operation name
   * @param status gRPC status code
   * @param message grpc-message, may be null
   * @param secrets secret values to remove
   * @return the exception of the matching subclass
   */
  public static OtelCollectorException grpcError(
      String operation, int status, String message, Collection<String> secrets) {
    ErrorCode code = codeForGrpcStatus(status);
    String text =
        message == null || message.isBlank()
            ? grpcStatusName(status).toLowerCase(java.util.Locale.ROOT).replace('_', ' ')
            : message.replaceAll("\\s+", " ").trim();
    text = Redaction.truncate(Redaction.redactText(text, secrets), Redaction.MAX_VENDOR_TEXT);
    return OtelCollectorException.of(
        code,
        operation + ": " + text + " (gRPC " + grpcStatusName(status) + ")",
        null,
        null,
        null,
        null);
  }

  /**
   * Converts a transport failure without a response (connection refused, timeout) into an SDK
   * exception.
   *
   * @param operation operation name
   * @param error the failure
   * @param secrets secret values to remove
   * @return {@link TimeoutException} for timeouts, else {@link UnavailableException}
   */
  public static OtelCollectorException networkError(
      String operation, Throwable error, Collection<String> secrets) {
    if (error instanceof OtelCollectorException) {
      return (OtelCollectorException) error;
    }
    String what = error == null ? "export failed" : error.getClass().getSimpleName();
    String detail =
        error == null || error.getMessage() == null
            ? ""
            : ": " + Redaction.truncate(Redaction.redactText(error.getMessage(), secrets), 200);
    if (error instanceof SocketTimeoutException || error instanceof InterruptedIOException) {
      return new TimeoutException(operation + ": request timed out (" + what + ")" + detail);
    }
    return new UnavailableException(operation + ": network error (" + what + ")" + detail);
  }

  /**
   * Exporter retry policy derived from the SDK retry policy ({@code maxRetries + 1} attempts,
   * clamped to 2..5 as the OpenTelemetry exporters require); empty when retries are off.
   *
   * @param retry SDK retry policy
   * @return the exporter policy, or empty
   */
  public static Optional<io.opentelemetry.sdk.common.export.RetryPolicy> exporterRetryPolicy(
      RetryPolicy retry) {
    if (retry.maxRetries() <= 0) {
      return Optional.empty();
    }
    return Optional.of(
        io.opentelemetry.sdk.common.export.RetryPolicy.builder()
            .setMaxAttempts(Math.min(5, retry.maxRetries() + 1))
            .setInitialBackoff(retry.initialBackoff())
            .setMaxBackoff(retry.maxBackoff())
            .setBackoffMultiplier(Math.max(1.0, retry.multiplier()))
            .build());
  }

  /**
   * Applies {@link TlsOptions} to an OpenTelemetry exporter builder: trusted CA certificates,
   * client certificate and key, or a trust-all context for {@code insecureSkipVerify} (tests only).
   *
   * @param tls TLS options
   * @param sslContext the builder's {@code setSslContext}
   * @param trusted the builder's {@code setTrustedCertificates}
   * @param clientTls the builder's {@code setClientTls(key, cert)}
   * @throws ConfigException when a file cannot be read
   */
  public static void applyTls(
      TlsOptions tls,
      BiConsumer<SSLContext, X509TrustManager> sslContext,
      Consumer<byte[]> trusted,
      BiConsumer<byte[], byte[]> clientTls) {
    if (!tls.isCustom()) {
      return;
    }
    if (tls.insecureSkipVerify()) {
      sslContext.accept(tls.sslContext(), trustManager(tls));
      return;
    }
    caPem(tls).ifPresent(trusted);
    if (tls.clientCertPath().isPresent() && tls.clientKeyPath().isPresent()) {
      clientTls.accept(
          read(tls.clientKeyPath().get(), "client key"),
          read(tls.clientCertPath().get(), "client certificate"));
    }
  }

  private static Optional<byte[]> caPem(TlsOptions tls) {
    if (tls.caCertPem().isPresent()) {
      return Optional.of(tls.caCertPem().get().getBytes(StandardCharsets.UTF_8));
    }
    return tls.caCertPath().map(p -> read(p, "CA certificate"));
  }

  private static byte[] read(Path path, String what) {
    try {
      return Files.readAllBytes(path);
    } catch (IOException e) {
      throw new ConfigException("otlp: cannot read the " + what + " file", e);
    }
  }

  /**
   * The trust manager matching {@link TlsOptions}: trust-all for {@code insecureSkipVerify}, the
   * configured CA bundle, else the system default.
   *
   * @param tls TLS options
   * @return the trust manager
   * @throws ConfigException when the CA bundle cannot be read or parsed
   */
  public static X509TrustManager trustManager(TlsOptions tls) {
    if (tls.insecureSkipVerify()) {
      return TRUST_ALL;
    }
    try {
      KeyStore ks = null;
      Optional<byte[]> ca = caPem(tls);
      if (ca.isPresent()) {
        ks = KeyStore.getInstance("PKCS12");
        ks.load(null, null);
        CertificateFactory cf = CertificateFactory.getInstance("X.509");
        int i = 0;
        for (Certificate c : cf.generateCertificates(new ByteArrayInputStream(ca.get()))) {
          ks.setCertificateEntry("ca-" + i++, c);
        }
      }
      TrustManagerFactory tmf =
          TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
      tmf.init(ks);
      for (TrustManager tm : tmf.getTrustManagers()) {
        if (tm instanceof X509TrustManager) {
          return (X509TrustManager) tm;
        }
      }
      throw new ConfigException("otlp: no X509 trust manager available");
    } catch (GeneralSecurityException | IOException e) {
      throw new ConfigException("otlp: cannot load the CA certificate", e);
    }
  }

  private static final X509TrustManager TRUST_ALL =
      new X509TrustManager() {
        @Override
        public void checkClientTrusted(X509Certificate[] chain, String authType) {}

        @Override
        public void checkServerTrusted(X509Certificate[] chain, String authType) {}

        @Override
        public X509Certificate[] getAcceptedIssuers() {
          return new X509Certificate[0];
        }
      };
}
