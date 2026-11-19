package codes.grow.packages.otelcollector;

import codes.grow.packages.otelcollector.core.ConfigException;
import io.opentelemetry.common.ComponentLoader;
import io.opentelemetry.sdk.common.CompletableResultCode;
import io.opentelemetry.sdk.common.export.Compressor;
import io.opentelemetry.sdk.common.export.GrpcResponse;
import io.opentelemetry.sdk.common.export.GrpcSender;
import io.opentelemetry.sdk.common.export.GrpcSenderConfig;
import io.opentelemetry.sdk.common.export.GrpcSenderProvider;
import io.opentelemetry.sdk.common.export.HttpResponse;
import io.opentelemetry.sdk.common.export.HttpSender;
import io.opentelemetry.sdk.common.export.HttpSenderProvider;
import io.opentelemetry.sdk.common.export.RetryPolicy;
import java.net.URI;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.function.Consumer;
import java.util.function.Supplier;
import javax.net.ssl.SSLContext;
import javax.net.ssl.X509TrustManager;

/**
 * Access to the OpenTelemetry exporter senders (public SPI of {@code opentelemetry-sdk-common}): a
 * {@link ComponentLoader} whose HTTP and gRPC senders report the final response of every export
 * (the official exporters drop partial-success bodies), and a raw gRPC unary call for {@code ping}.
 */
final class OtlpSenders {
  private OtlpSenders() {}

  /** Receives the outcome of each send: an {@link HttpResponse}, a {@link GrpcResponse} or null. */
  interface Sink {
    void accept(Object response, Throwable error);
  }

  private static ComponentLoader base() {
    return ComponentLoader.forClassLoader(OtlpSenders.class.getClassLoader());
  }

  /**
   * A component loader for exporter builders whose senders report every outcome to {@code sink}.
   *
   * @param sink receives responses and errors
   * @return the loader
   */
  static ComponentLoader capturing(Sink sink) {
    ComponentLoader base = base();
    return new ComponentLoader() {
      @Override
      @SuppressWarnings("unchecked")
      public <T> Iterable<T> load(Class<T> spi) {
        Iterable<T> loaded = base.load(spi);
        if (spi != HttpSenderProvider.class && spi != GrpcSenderProvider.class) {
          return loaded;
        }
        List<T> out = new ArrayList<>();
        for (T p : loaded) {
          if (p instanceof HttpSenderProvider) {
            HttpSenderProvider hp = (HttpSenderProvider) p;
            out.add(
                (T)
                    (HttpSenderProvider)
                        cfg -> new CapturingHttpSender(hp.createSender(cfg), sink));
          } else {
            GrpcSenderProvider gp = (GrpcSenderProvider) p;
            out.add(
                (T)
                    (GrpcSenderProvider)
                        cfg -> new CapturingGrpcSender(gp.createSender(cfg), sink));
          }
        }
        return out;
      }
    };
  }

  private static final class CapturingHttpSender implements HttpSender {
    private final HttpSender delegate;
    private final Sink sink;

    CapturingHttpSender(HttpSender delegate, Sink sink) {
      this.delegate = delegate;
      this.sink = sink;
    }

    @Override
    public void send(
        io.opentelemetry.sdk.common.export.MessageWriter writer,
        Consumer<HttpResponse> onResponse,
        Consumer<Throwable> onError) {
      delegate.send(
          writer,
          r -> {
            sink.accept(r, null);
            onResponse.accept(r);
          },
          e -> {
            sink.accept(null, e);
            onError.accept(e);
          });
    }

    @Override
    public CompletableResultCode shutdown() {
      return delegate.shutdown();
    }
  }

  private static final class CapturingGrpcSender implements GrpcSender {
    private final GrpcSender delegate;
    private final Sink sink;

    CapturingGrpcSender(GrpcSender delegate, Sink sink) {
      this.delegate = delegate;
      this.sink = sink;
    }

    @Override
    public void send(
        io.opentelemetry.sdk.common.export.MessageWriter writer,
        Consumer<GrpcResponse> onResponse,
        Consumer<Throwable> onError) {
      delegate.send(
          writer,
          r -> {
            sink.accept(r, null);
            onResponse.accept(r);
          },
          e -> {
            sink.accept(null, e);
            onError.accept(e);
          });
    }

    @Override
    public CompletableResultCode shutdown() {
      return delegate.shutdown();
    }
  }

  /**
   * Creates a gRPC sender (the exporter's own HTTP/2 stack) for one unary method.
   *
   * @param endpoint {@code http://host:port} or {@code https://host:port}
   * @param method full method name {@code package.Service/Method}
   * @param timeout per-call deadline
   * @param connectTimeout connect timeout
   * @param headers metadata
   * @param retry exporter retry policy, may be null
   * @param sslContext TLS context for https, may be null
   * @param trustManager trust manager for https, may be null
   * @return the sender
   * @throws ConfigException when no gRPC sender is on the class path
   */
  static GrpcSender grpcSender(
      URI endpoint,
      String method,
      Duration timeout,
      Duration connectTimeout,
      Map<String, List<String>> headers,
      RetryPolicy retry,
      SSLContext sslContext,
      X509TrustManager trustManager) {
    Iterator<GrpcSenderProvider> it = base().load(GrpcSenderProvider.class).iterator();
    if (!it.hasNext()) {
      throw new ConfigException("ping: no OpenTelemetry gRPC sender on the class path");
    }
    GrpcSenderConfig cfg =
        new GrpcSenderConfig() {
          @Override
          public URI getEndpoint() {
            return endpoint;
          }

          @Override
          public String getFullMethodName() {
            return method;
          }

          @Override
          public Compressor getCompressor() {
            return null;
          }

          @Override
          public Duration getTimeout() {
            return timeout;
          }

          @Override
          public Duration getConnectTimeout() {
            return connectTimeout;
          }

          @Override
          public Supplier<Map<String, List<String>>> getHeadersSupplier() {
            return () -> headers;
          }

          @Override
          public RetryPolicy getRetryPolicy() {
            return retry;
          }

          @Override
          public SSLContext getSslContext() {
            return sslContext;
          }

          @Override
          public X509TrustManager getTrustManager() {
            return trustManager;
          }

          @Override
          public ExecutorService getExecutorService() {
            return null;
          }
        };
    return it.next().createSender(cfg);
  }
}
