# Configuration

`OtelCollectorConfig` holds every setting of a client, of `initTelemetry` and of `initTracing`. It
is **immutable** after construction, validated when it is built (invalid values raise a `CONFIG`
error) and its string/debug form **redacts every credential** (header values such as
`Authorization`, user info in URLs).

The collector itself needs no credentials. Put gateway or `bearertokenauth` credentials in
`headers` (`OTEL_EXPORTER_OTLP_HEADERS`); they are sent with every export and with `ping()`, never
to the unauthenticated admin endpoints (`health_check`, zpages, internal telemetry).

## Precedence

`fromEnv` resolves each setting in this order:

1. the value passed in code (overrides),
2. `GROW_OPENTELEMETRY_COLLECTOR_<NAME>`,
3. `GROW_OTEL_<NAME>` (alias),
4. the per-signal standard variable (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, …),
5. the generic standard variable (`OTEL_EXPORTER_OTLP_ENDPOINT`, …),
6. the default.

Map settings (`headers`, `resourceAttributes`, per-signal headers, batch settings) merge key by key
in the same order. `OTEL_EXPORTER_OTLP_ENDPOINT` feeds `url` for `http/protobuf` and `grpcUrl` for
`grpc`.

Parsing rules:

- **Durations** accept integer milliseconds (`1500`) or units: `500ms`, `10s`, `2m`, `1m30s`. The
  standard `OTEL_*` durations are integer milliseconds, as in the OpenTelemetry specification.
- **Booleans** accept `true/false`, `1/0`, `yes/no`, `on/off` (any case).
- **Key-value lists** (`HEADERS`, `RESOURCE_ATTRIBUTES`) are `k=v,k2=v2` with percent-encoded values.
- An **empty** variable counts as unset.
- A present but **invalid** value raises a `CONFIG` error that names the variable, never its value.

## OTLP and collector settings

| Setting | `GROW_OPENTELEMETRY_COLLECTOR_…` / `GROW_OTEL_…` | Standard variable | Default |
| - | - | - | - |
| `url` — OTLP/HTTP base URL | `URL` | `OTEL_EXPORTER_OTLP_ENDPOINT` (protocol `http/protobuf`) | `http://127.0.0.1:4318` |
| `grpcUrl` — OTLP/gRPC target (`https://` = TLS) | `GRPC_URL` | `OTEL_EXPORTER_OTLP_ENDPOINT` (protocol `grpc`) | scheme and host of `url`, port 4317 |
| `protocol` | `PROTOCOL` | `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` (`grpc`; `native` → `CONFIG`) |
| `tracesUrl`, `logsUrl`, `metricsUrl` — per-signal endpoint, used as is (both protocols) | `TRACES_URL`, `LOGS_URL`, `METRICS_URL` | `OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT` | `url` + `/v1/<signal>`, or `grpcUrl` |
| `headers` — sent with every export and `ping()` (redacted) | `HEADERS` | `OTEL_EXPORTER_OTLP_HEADERS` | none |
| `tracesHeaders`, `logsHeaders`, `metricsHeaders` | — | `OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS` | none |
| `compression` | `COMPRESSION` | `OTEL_EXPORTER_OTLP_COMPRESSION` | `gzip` (`none`) |
| `exportTimeout` — one export incl. the exporter's retries | `EXPORT_TIMEOUT` | `OTEL_EXPORTER_OTLP_TIMEOUT` | 10 s |
| `healthUrl` — `health_check` extension | `HEALTH_URL` | — | host of `url`, port 13133, path `/` |
| `zpagesUrl` — zpages extension | `ZPAGES_URL` | — | host of `url`, port 55679 |
| `telemetryUrl` — internal telemetry (`:8888/metrics`) for `serverVersion()` | `TELEMETRY_URL` | — | none |
| `serviceName` | `SERVICE_NAME` | `OTEL_SERVICE_NAME`, `service.name` in `OTEL_RESOURCE_ATTRIBUTES` | `unknown_service:<process>` |
| `serviceVersion` | `SERVICE_VERSION` | — | none |
| `environment` (`deployment.environment.name`) | `ENVIRONMENT` | — | none |
| `resourceAttributes` | `RESOURCE_ATTRIBUTES` | `OTEL_RESOURCE_ATTRIBUTES` | none |
| `resourceDetectors` | `RESOURCE_DETECTORS` | — | `host,process,container,k8s` (`none` = off) |
| `traces`, `logs`, `metrics` — signal switches | `TRACES`, `LOGS`, `METRICS` | `OTEL_TRACES_EXPORTER=none` (…), `OTEL_SDK_DISABLED=true` | `true` |
| `sampler` | `SAMPLER` | `OTEL_TRACES_SAMPLER` | `parentbased_always_on` |
| `samplerRatio` | `SAMPLER_RATIO` | `OTEL_TRACES_SAMPLER_ARG` | 1.0 |
| `propagators` | `PROPAGATORS` | `OTEL_PROPAGATORS` | `tracecontext,baggage` (also `b3`, `b3multi`) |
| `tracesBatch` — batch span processor | — | `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_EXPORT_TIMEOUT`, `OTEL_BSP_MAX_QUEUE_SIZE`, `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` | SDK defaults |
| `logsBatch` — log batch processor | — | `OTEL_BLRP_SCHEDULE_DELAY`, `OTEL_BLRP_EXPORT_TIMEOUT`, `OTEL_BLRP_MAX_QUEUE_SIZE`, `OTEL_BLRP_MAX_EXPORT_BATCH_SIZE` | SDK defaults |
| `metricExportInterval` | `METRIC_EXPORT_INTERVAL` | `OTEL_METRIC_EXPORT_INTERVAL` | 60 s |
| `runtimeMetrics` | `RUNTIME_METRICS` | — | `false` |

## Transport settings

These apply to `health()`, `ping()` and `serverVersion()` (the shared HTTP transport); TLS,
proxy-free connections, headers and the User-Agent also apply to the OTLP exporters.

| Setting | `GROW_OPENTELEMETRY_COLLECTOR_…` / `GROW_OTEL_…` | Standard variable | Default |
| - | - | - | - |
| `connectTimeout` | `CONNECT_TIMEOUT` | — | 10 s |
| `requestTimeout` (per attempt) | `REQUEST_TIMEOUT` | — | 30 s |
| `totalTimeout` (whole call incl. retries) | `TOTAL_TIMEOUT` | — | 120 s |
| `retry.maxRetries` | `MAX_RETRIES` | — | 3 |
| `tls.caCertPath` | `CA_CERT` | `OTEL_EXPORTER_OTLP_CERTIFICATE` | system roots |
| `tls.clientCertPath` / `tls.clientKeyPath` | `CLIENT_CERT` / `CLIENT_KEY` | `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` / `OTEL_EXPORTER_OTLP_CLIENT_KEY` | none |
| `tls.insecureSkipVerify` (tests only; `CONFIG` with gRPC in Python and Rust) | `INSECURE_SKIP_VERIFY` | — | `false` |
| `proxy` | `PROXY` | — | none (`HTTP_PROXY`/`HTTPS_PROXY` are ignored) |
| `userAgentSuffix`, `hooks`, `logger` | — | — | none / silent |

Every request sends `User-Agent: grow-codes-opentelemetry-collector/<version> (<language>)`.

## Examples

=== "TypeScript"

    ```ts
    import { OtelCollectorClient, OtelCollectorConfig } from "@grow-codes/opentelemetry-collector";

    // Environment only
    const client = OtelCollectorClient.fromEnv();

    // Environment with explicit overrides (explicit values win)
    const config = OtelCollectorConfig.fromEnv({ serviceName: "checkout", protocol: "grpc" });

    // Everything in code
    const explicit = new OtelCollectorConfig({
      url: "https://otel.example.com:4318",
      protocol: "http/protobuf",
      headers: { authorization: `Bearer ${process.env.OTEL_GATEWAY_TOKEN}` },
      serviceName: "checkout",
      serviceVersion: "1.4.0",
      environment: "staging",
      resourceAttributes: { team: "payments" },
      sampler: "parentbased_traceidratio",
      samplerRatio: 0.25,
      tls: { caCertPath: "/etc/ssl/internal-ca.pem" },
      requestTimeoutMs: 5_000,
    });
    console.log(String(explicit)); // authorization shown as [REDACTED]
    ```

=== "Rust"

    ```rust
    use grow_opentelemetry_collector::tracing::{Protocol, SamplerKind};
    use grow_opentelemetry_collector::{OtelCollectorClient, OtelCollectorConfig};

    // Environment only
    let client = OtelCollectorClient::from_env()?;

    // Environment with explicit overrides (explicit values win)
    let config = OtelCollectorConfig::builder()
        .service_name("checkout")
        .protocol(Protocol::Grpc)
        .build_from_env()?;

    // Everything in code
    let explicit = OtelCollectorConfig::builder()
        .url("https://otel.example.com:4318")
        .header("authorization", format!("Bearer {}", std::env::var("OTEL_GATEWAY_TOKEN").unwrap_or_default()))
        .service_name("checkout")
        .service_version("1.4.0")
        .environment("staging")
        .sampler(SamplerKind::ParentBasedTraceIdRatio, 0.25)
        .ca_cert_path("/etc/ssl/internal-ca.pem")
        .build()?;
    println!("{explicit:?}"); // authorization shown as [REDACTED]
    ```

=== "Go"

    ```go
    // Environment only
    cfg, err := otelcollector.ConfigFromEnv()

    // Environment with explicit overrides (explicit values win; nil env = process environment)
    cfg, err = otelcollector.ConfigFromEnvWith(otelcollector.Config{
        ServiceName: "checkout",
        Protocol:    otelcollector.ProtocolGRPC,
    }, nil)

    // Everything in code: zero values mean "use the default"
    explicit := otelcollector.Config{
        CommonConfig: otelcollector.CommonConfig{
            URL:     "https://otel.example.com:4318",
            Headers: map[string]string{"authorization": "Bearer " + os.Getenv("OTEL_GATEWAY_TOKEN")},
            TLS:     otelcollector.TLSConfig{CACertPath: "/etc/ssl/internal-ca.pem"},
        },
        ServiceName:    "checkout",
        ServiceVersion: "1.4.0",
        Environment:    "staging",
        Sampler:        otelcollector.SamplerParentBasedTraceIDRatio,
        SamplerRatio:   0.25,
    }
    if err := explicit.Validate(); err != nil {
        return err // errors.Is(err, otelcollector.ErrConfig)
    }
    fmt.Println(explicit) // authorization shown as [REDACTED]
    ```

=== "Python"

    ```python
    import os
    from grow_opentelemetry_collector import OtelCollectorClient, OtelCollectorConfig, TlsConfig

    # Environment only
    client = OtelCollectorClient.from_env()

    # Environment with explicit overrides (explicit values win)
    config = OtelCollectorConfig.from_env(service_name="checkout", protocol="grpc")

    # Everything in code: durations are float seconds (datetime.timedelta also works)
    explicit = OtelCollectorConfig(
        url="https://otel.example.com:4318",
        headers={"authorization": f"Bearer {os.environ.get('OTEL_GATEWAY_TOKEN', '')}"},
        service_name="checkout",
        service_version="1.4.0",
        environment="staging",
        sampler="parentbased_traceidratio",
        sampler_ratio=0.25,
        tls=TlsConfig(ca_cert_path="/etc/ssl/internal-ca.pem"),
        request_timeout=5.0,
    )
    print(repr(explicit))  # authorization shown as [REDACTED]
    ```

=== "Java"

    ```java
    // Environment only
    OtelCollectorConfig fromEnv = OtelCollectorConfig.fromEnv();

    // Everything in code
    OtelCollectorConfig config = OtelCollectorConfig.builder()
        .url("https://otel.example.com:4318")
        .protocol(Protocol.HTTP_PROTOBUF)
        .header("authorization", "Bearer " + System.getenv("OTEL_GATEWAY_TOKEN"))
        .serviceName("checkout")
        .serviceVersion("1.4.0")
        .environment("staging")
        .sampler(SamplerType.PARENTBASED_TRACEIDRATIO)
        .samplerRatio(0.25)
        .requestTimeout(Duration.ofSeconds(5))
        .build();
    System.out.println(config); // authorization shown as [REDACTED]
    ```

## Retry policy

| Field | Default | Meaning |
| - | - | - |
| `maxRetries` | 3 | retries after the first attempt (so up to 4 attempts) |
| `initialBackoff` | 100 ms | backoff cap of the first retry |
| `maxBackoff` | 10 s | cap of every backoff |
| `multiplier` | 2 | growth of the cap per retry |
| `maxRetryAfter` | 60 s | longest server-requested `Retry-After` the client waits for |

Backoff for retry *n* (0-based) is **full jitter**: a random wait between 0 and
`min(maxBackoff, initialBackoff × multiplier^n)`. The policy applies to `ping()` and
`serverVersion()` (`health()` never retries). OTLP exports are retried by the official exporters
(HTTP 429/502/503/504 and the retryable gRPC codes, honouring `Retry-After`) within `exportTimeout`.

## Timeouts and cancellation

- `connectTimeout` bounds connection setup, `requestTimeout` each attempt, `totalTimeout` the whole
  call including backoff sleeps. An exhausted timeout raises `TIMEOUT`.
- Cancellation is honoured during requests and backoff sleeps and raises `CANCELLED`:
  TypeScript `{ signal }` (`AbortSignal`), Rust dropping the future, Go `ctx`, Python `cancel=`
  (`CancelToken`) or task cancellation, Java `…Async` futures (`cancel(true)`).

## TLS and proxies

- An `https://` URL enables TLS. Add a private CA with `tls.caCertPath`; mutual TLS needs
  `tls.clientCertPath` and `tls.clientKeyPath` together.
- `tls.insecureSkipVerify` disables certificate checks for local tests only (the client logs a
  warning once). With `grpc` it is a `CONFIG` error in Python and Rust (grpcio and tonic cannot skip
  verification).
- `HTTP_PROXY`/`HTTPS_PROXY` are **ignored** so that credentials never leave through an unexpected
  proxy. Set `proxy` (or `GROW_OPENTELEMETRY_COLLECTOR_PROXY`) to use one.
