package codes.grow.packages.otelcollector;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;

import codes.grow.packages.otelcollector.core.AuthenticationException;
import codes.grow.packages.otelcollector.core.ConfigException;
import codes.grow.packages.otelcollector.core.ErrorCode;
import codes.grow.packages.otelcollector.core.OtelCollectorException;
import codes.grow.packages.otelcollector.core.RetryPolicy;
import codes.grow.packages.otelcollector.core.TimeoutException;
import codes.grow.packages.otelcollector.core.TlsOptions;
import codes.grow.packages.otelcollector.core.UnavailableException;
import codes.grow.packages.otelcollector.core.ValidationException;
import java.io.IOException;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.HexFormat;
import java.util.List;
import java.util.concurrent.atomic.AtomicReference;
import javax.net.ssl.X509TrustManager;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class OtlpTest {
  private static byte[] hex(String h) {
    return HexFormat.of().parseHex(h);
  }

  @Test
  void decodeRpcStatusFromFixtures() {
    assertThat(Otlp.decodeRpcStatus(TestSupport.body("ping.http_unauthenticated_missing_token")))
        .contains(new Otlp.RpcStatus(16, "missing or empty authorization header: Authorization"));
    assertThat(Otlp.decodeRpcStatus(TestSupport.body("export.http_bad_request_protobuf")))
        .get()
        .extracting(Otlp.RpcStatus::code)
        .isEqualTo(3);
    assertThat(Otlp.decodeRpcStatus(new byte[0])).isEmpty();
    assertThat(Otlp.decodeRpcStatus(null)).isEmpty();
    assertThat(Otlp.decodeRpcStatus(hex("ff"))).isEmpty();
    assertThat(Otlp.decodeRpcStatus(hex("1205ab"))).isEmpty();
    assertThat(Otlp.decodeRpcStatus(hex("0f"))).isEmpty();
    assertThat(Otlp.decodeRpcStatus(hex("0d000000001100000000000000000808")))
        .contains(new Otlp.RpcStatus(8, ""));
    assertThat(Otlp.decodeRpcStatus(hex("08ffffffffffffffffffff"))).isEmpty();
  }

  @Test
  void decodePartialSuccess() {
    assertThat(Otlp.decodePartialSuccess(hex("0a00"))).isEqualTo(new Otlp.PartialSuccess(0, ""));
    assertThat(Otlp.decodePartialSuccess(hex("0a050802120178")))
        .isEqualTo(new Otlp.PartialSuccess(2, "x"));
    assertThat(Otlp.decodePartialSuccess(null)).isEqualTo(Otlp.PartialSuccess.NONE);
    assertThat(Otlp.decodePartialSuccess(hex("1000"))).isEqualTo(Otlp.PartialSuccess.NONE);
    assertThat(Otlp.decodePartialSuccess(hex("ff"))).isEqualTo(Otlp.PartialSuccess.NONE);
    assertThat(Otlp.decodePartialSuccess(hex("0a01ff"))).isEqualTo(Otlp.PartialSuccess.NONE);
  }

  @Test
  void errorText() {
    assertThat(Otlp.errorText(TestSupport.body("export.http_bad_request_json")))
        .get()
        .asString()
        .contains("ReadObject");
    assertThat(Otlp.errorText("404 page not found\n".getBytes(StandardCharsets.UTF_8)))
        .contains("404 page not found");
    assertThat(Otlp.errorText("{not json".getBytes(StandardCharsets.UTF_8))).contains("{not json");
    assertThat(Otlp.errorText("{\"code\":3}".getBytes(StandardCharsets.UTF_8)))
        .contains("{\"code\":3}");
    assertThat(Otlp.errorText(hex("00ff01"))).isEmpty();
    assertThat(Otlp.errorText("   ".getBytes(StandardCharsets.UTF_8))).isEmpty();
    assertThat(Otlp.errorText(null)).isEmpty();
    assertThat(Otlp.errorText(hex("0803"))).isEmpty();
  }

  @Test
  void httpError() {
    OtelCollectorException e =
        Otlp.httpError(
            "logs.export",
            413,
            TestSupport.body("export.http_payload_too_large"),
            List.of("secret"));
    assertThat(e).isInstanceOf(ValidationException.class);
    assertThat(e.httpStatus()).hasValue(413);
    assertThat(e.getMessage()).contains("request body too large").contains("HTTP 413");
    assertThat(Otlp.httpError("ping", 401, null, List.of()))
        .isInstanceOf(AuthenticationException.class)
        .hasMessage("ping: unauthorized (HTTP 401)");
  }

  @Test
  void grpcCodes() {
    int[] codes = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 99};
    ErrorCode[] want = {
      ErrorCode.CANCELLED,
      ErrorCode.UNAVAILABLE,
      ErrorCode.VALIDATION,
      ErrorCode.TIMEOUT,
      ErrorCode.NOT_FOUND,
      ErrorCode.CONFLICT,
      ErrorCode.PERMISSION_DENIED,
      ErrorCode.RATE_LIMITED,
      ErrorCode.VALIDATION,
      ErrorCode.CONFLICT,
      ErrorCode.VALIDATION,
      ErrorCode.NOT_FOUND,
      ErrorCode.VALIDATION,
      ErrorCode.UNAVAILABLE,
      ErrorCode.UNAVAILABLE,
      ErrorCode.AUTHENTICATION,
      ErrorCode.UNAVAILABLE
    };
    for (int i = 0; i < codes.length; i++) {
      assertThat(Otlp.codeForGrpcStatus(codes[i])).isEqualTo(want[i]);
    }
    assertThat(Otlp.grpcStatusName(16)).isEqualTo("UNAUTHENTICATED");
    assertThat(Otlp.grpcStatusName(-1)).isEqualTo("-1");
    assertThat(Otlp.grpcError("ping", 14, null, List.of()))
        .isInstanceOf(UnavailableException.class)
        .hasMessage("ping: unavailable (gRPC UNAVAILABLE)");
    assertThat(Otlp.grpcError("ping", 16, "token tok-xyz-123 bad", List.of("tok-xyz-123")))
        .hasMessageNotContaining("tok-xyz-123");
  }

  @Test
  void networkError() {
    OtelCollectorException same = new ValidationException("x");
    assertThat(Otlp.networkError("op", same, List.of())).isSameAs(same);
    assertThat(Otlp.networkError("op", new SocketTimeoutException("slow"), List.of()))
        .isInstanceOf(TimeoutException.class)
        .hasMessageContaining("slow");
    assertThat(Otlp.networkError("op", new IOException("refused s3cr3t"), List.of("s3cr3t")))
        .isInstanceOf(UnavailableException.class)
        .hasMessageNotContaining("s3cr3t");
    assertThat(Otlp.networkError("op", new IOException(), List.of()))
        .hasMessage("op: network error (IOException)");
    assertThat(Otlp.networkError("op", null, List.of())).hasMessageContaining("export failed");
  }

  @Test
  void exporterRetryPolicy() {
    assertThat(Otlp.exporterRetryPolicy(RetryPolicy.none())).isEmpty();
    io.opentelemetry.sdk.common.export.RetryPolicy p =
        Otlp.exporterRetryPolicy(
                RetryPolicy.builder()
                    .maxRetries(10)
                    .initialBackoff(Duration.ofMillis(5))
                    .maxBackoff(Duration.ofMillis(50))
                    .build())
            .get();
    assertThat(p.getMaxAttempts()).isEqualTo(5);
    assertThat(p.getInitialBackoff()).isEqualTo(Duration.ofMillis(5));
  }

  @Test
  void applyTlsAndTrustManager(@TempDir Path dir) throws Exception {
    AtomicReference<Object> ssl = new AtomicReference<>();
    AtomicReference<byte[]> trusted = new AtomicReference<>();
    AtomicReference<byte[]> key = new AtomicReference<>();
    Otlp.applyTls(TlsOptions.defaults(), (c, t) -> ssl.set(c), trusted::set, (k, c) -> key.set(k));
    assertThat(ssl.get()).isNull();
    Otlp.applyTls(
        TlsOptions.builder().insecureSkipVerify(true).build(),
        (c, t) -> ssl.set(t),
        trusted::set,
        (k, c) -> key.set(k));
    assertThat(ssl.get()).isInstanceOf(X509TrustManager.class);
    X509TrustManager all = (X509TrustManager) ssl.get();
    all.checkClientTrusted(null, "RSA");
    all.checkServerTrusted(null, "RSA");
    assertThat(all.getAcceptedIssuers()).isEmpty();

    String pem = TestSupport.selfSignedPem(dir);
    Path ca = dir.resolve("ca.pem");
    Files.writeString(ca, pem);
    Path cert = dir.resolve("c.pem");
    Path k = dir.resolve("k.pem");
    Files.writeString(cert, "CERT");
    Files.writeString(k, "KEY");
    Otlp.applyTls(
        TlsOptions.builder().caCertPath(ca).clientCertPath(cert).clientKeyPath(k).build(),
        (c, t) -> ssl.set(c),
        trusted::set,
        (kk, c) -> key.set(kk));
    assertThat(new String(trusted.get(), StandardCharsets.UTF_8)).isEqualTo(pem);
    assertThat(new String(key.get(), StandardCharsets.UTF_8)).isEqualTo("KEY");
    Otlp.applyTls(
        TlsOptions.builder().caCertPem(pem).build(),
        (c, t) -> ssl.set(c),
        trusted::set,
        (kk, c) -> key.set(kk));
    assertThat(Otlp.trustManager(TlsOptions.builder().caCertPem(pem).build()).getAcceptedIssuers())
        .hasSize(1);
    assertThat(Otlp.trustManager(TlsOptions.defaults()).getAcceptedIssuers()).isNotEmpty();
    assertThatThrownBy(
            () ->
                Otlp.applyTls(
                    TlsOptions.builder().caCertPath(dir.resolve("missing.pem")).build(),
                    (c, t) -> {},
                    b -> {},
                    (a, b) -> {}))
        .isInstanceOf(ConfigException.class);
    assertThatThrownBy(() -> Otlp.trustManager(TlsOptions.builder().caCertPem("garbage").build()))
        .isInstanceOf(ConfigException.class);
  }
}
