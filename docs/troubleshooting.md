# Troubleshooting

## The client cannot connect

- `health()` returns `healthy: false, status: "unreachable"`: the URL is wrong or the service is
  down. Check `GROW_OPENTELEMETRY_COLLECTOR_URL` (default `http://127.0.0.1:4318`) and that the port is
  reachable from where your code runs (inside Docker, `127.0.0.1` is the container itself).
- Calls fail with `UNAVAILABLE` after several seconds: the client retried with backoff. Lower
  `maxRetries` or `totalTimeout` to fail faster.
- Behind a corporate proxy: `HTTP_PROXY`/`HTTPS_PROXY` are **ignored** on purpose. Set `proxy`
  (or `GROW_OPENTELEMETRY_COLLECTOR_PROXY`).

## `CONFIG` errors

The message names the setting or environment variable, never its value. Typical causes: a URL
without `http://` or `https://`, a duration such as `10 sec` (use `10s` or `10000`), a client
certificate without its key, `MAX_RETRIES` below 0.

## `AUTHENTICATION` or `PERMISSION_DENIED`

- The collector needs no credentials by default. These errors come from an authenticator extension
  (for example `bearertokenauth`) or a gateway in front of the collector.
- `ping()` is the quickest check: it sends an empty OTLP export with the configured headers.
- Put credentials in `headers` (`OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer%20<token>"` or
  `GROW_OPENTELEMETRY_COLLECTOR_HEADERS`); values are percent-encoded. Explicit config values win
  over the environment.
- The SDK never prints credentials, so `toString()` of the config shows `[REDACTED]` for header
  values.

## `health()` says `zpages` or `unreachable`

- `details.source = "zpages"`: `health_check` on port 13133 was unreachable and zpages answered.
  This is normal for collectors with the image's built-in configuration (Grow stacks), which bind
  `health_check` to `localhost`. Bind it to `0.0.0.0:13133` to use it directly.
- `status = "unreachable"`: neither `healthUrl` (default host of `url`, port 13133) nor `zpagesUrl`
  (port 55679) answered. Set `healthUrl` / `zpagesUrl` when the admin ports are mapped elsewhere.
- `serverVersion()` raises `UNSUPPORTED` when zpages is off; set `telemetryUrl` to the internal
  telemetry endpoint (`http://<collector>:8888/metrics`) to read `target_info` instead.

## Nothing arrives at the collector

- Telemetry is batched: call `forceFlush()` (or `shutdown()` / `client.close()`) before the
  process exits.
- `grpc` uses port **4317** (`grpcUrl`, default host of `url`), `http/protobuf` port **4318**. A
  per-signal URL (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, …) is used as is, in both protocols.
- `OTEL_SDK_DISABLED=true` or `OTEL_<SIGNAL>_EXPORTER=none` switches signals off.
- The Python gRPC exporter needs the `grpc` extra: `pip install "grow-opentelemetry-collector[grpc]"`.

## TLS errors

- Self-signed or private CA: set `tls.caCertPath` (`GROW_OPENTELEMETRY_COLLECTOR_CA_CERT` or
  `OTEL_EXPORTER_OTLP_CERTIFICATE`).
- Mutual TLS: set both `tls.clientCertPath` and `tls.clientKeyPath`.
- `tls.insecureSkipVerify` only for local tests; with `grpc` it is a `CONFIG` error in Python and
  Rust.

## `RATE_LIMITED`

The service asked the client to slow down. The client waits for `Retry-After` up to
`maxRetryAfter` (60 s); longer waits fail at once with `retryAfter` set on the error so your code
can schedule a later attempt.

## `TIMEOUT`

`requestTimeout` (30 s) bounds one attempt and `totalTimeout` (120 s) the whole call including
retries. Raise them for slow operations, or pass a per-call deadline (signal, ctx, cancel token,
future timeout).

## See what the client does

Turn on the [logger](hooks.md#logger) or add [hooks](hooks.md#hooks): both see every attempt with
redacted headers and URLs.

## Docker integration tests fail

- Run one language: `make test-docker LANGS=go`.
- In CI, download the `compose-logs-<lang>` artifact of the failed job.
- Locally, the stack is removed after each run; rerun with Docker logs open
  (`docker compose -f docker/compose.yml logs -f`) in a second terminal.

## Still stuck?

Open an issue with the language, package version, OpenTelemetry Collector version and a minimal reproduction
(with secrets removed): <https://github.com/grow-codes-packages/opentelemetry-collector/issues>.
